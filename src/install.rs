//! AUR install pipeline: resolve -> summary -> fetch -> review/audit ->
//! PGP keys -> build (makepkg) -> install (pacman -U), in dependency order.

use crate::args::Args;
use crate::pacman;
use crate::review;
use crate::rpc::{AurPkg, Client};
use crate::signals;
use crate::util::{self, bold, clone_dir, dim, green, msg};
use crate::vercmp::vercmp;
use anyhow::{Context, Result, bail};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

pub struct Opts {
    pub needed: bool,
    pub asdeps: bool,
    pub nocheck: bool,
    pub review: bool,
    pub clean: bool,
}

impl Opts {
    pub fn from_args(a: &Args) -> Self {
        Opts {
            needed: a.has_long("needed"),
            asdeps: a.has_long("asdeps"),
            nocheck: a.own("nocheck"),
            review: !a.own("noreview"),
            clean: a.own("clean"),
        }
    }
}

/// "foo>=1.2" -> "foo"
pub fn dep_name(dep: &str) -> &str {
    dep.split(['<', '>', '=']).next().unwrap_or(dep).trim()
}

fn provided_by<'a>(pkgs: &'a BTreeMap<String, AurPkg>, name: &str) -> Option<&'a AurPkg> {
    pkgs.values()
        .find(|p| p.name == name || p.provides.iter().any(|pr| dep_name(pr) == name))
}

/// For a dependency name not found as a pkgname: look for AUR packages
/// providing it (one RPC search, cached) and pick the most voted one.
fn find_provider(client: &mut Client, name: &str) -> Result<Option<AurPkg>> {
    let mut hits = client.search(name, "provides")?;
    hits.sort_by(|a, b| b.num_votes.cmp(&a.num_votes));
    let Some(best) = hits.into_iter().next() else {
        return Ok(None);
    };
    util::warn(&format!("using {} as the provider of {name}", best.name));
    Ok(client
        .info(std::slice::from_ref(&best.name))?
        .remove(&best.name))
}

struct Plan {
    pkgs: BTreeMap<String, AurPkg>,
    explicit: HashSet<String>,
    order: Vec<String>,
    repo_deps: BTreeSet<String>,
}

fn resolve(client: &mut Client, roots: Vec<AurPkg>, opts: &Opts) -> Result<Plan> {
    let explicit: HashSet<String> = roots.iter().map(|p| p.name.clone()).collect();
    let sync = pacman::sync_names()?;
    let mut pkgs: BTreeMap<String, AurPkg> = BTreeMap::new();
    let mut edges: HashMap<String, BTreeSet<String>> = HashMap::new();
    let mut repo_deps = BTreeSet::new();
    // dependency string -> AUR base providing it (None = system/repos)
    let mut provider: HashMap<String, Option<String>> = HashMap::new();
    let mut frontier = roots;

    while !frontier.is_empty() {
        for p in &frontier {
            pkgs.insert(p.name.clone(), p.clone());
        }
        let mut wanted: Vec<(String, String)> = Vec::new(); // (dep, requiring base)
        for p in &frontier {
            let check: &[String] = if opts.nocheck { &[] } else { &p.check_depends };
            for d in p.depends.iter().chain(&p.make_depends).chain(check) {
                wanted.push((d.clone(), p.package_base.clone()));
            }
        }
        let fresh: Vec<String> = wanted
            .iter()
            .map(|(d, _)| d.clone())
            .filter(|d| !provider.contains_key(d))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let unsat: HashSet<String> = pacman::unsatisfied(&fresh)?.into_iter().collect();

        let mut need_aur = Vec::new();
        for d in &fresh {
            let name = dep_name(d);
            if !unsat.contains(d) {
                provider.insert(d.clone(), None);
            } else if let Some(p) = provided_by(&pkgs, name) {
                provider.insert(d.clone(), Some(p.package_base.clone()));
            } else if sync.contains(name) || pacman::repo_satisfies(d) {
                repo_deps.insert(d.clone());
                provider.insert(d.clone(), None);
            } else {
                need_aur.push(d.clone());
            }
        }

        let names: Vec<String> = need_aur.iter().map(|d| dep_name(d).to_string()).collect();
        let found = client.info(&names)?;
        let mut next: Vec<AurPkg> = Vec::new();
        for d in &need_aur {
            let name = dep_name(d);
            let pkg = match found.get(name) {
                Some(p) => p.clone(),
                None => match find_provider(client, name)? {
                    Some(p) => p,
                    None => {
                        let by: Vec<&str> = wanted
                            .iter()
                            .filter(|(w, _)| w == d)
                            .map(|(_, b)| b.as_str())
                            .collect();
                        bail!(
                            "cannot resolve dependency '{d}' (required by {})",
                            by.join(", ")
                        );
                    }
                },
            };
            provider.insert(d.clone(), Some(pkg.package_base.clone()));
            if !pkgs.contains_key(&pkg.name) && !next.iter().any(|p| p.name == pkg.name) {
                next.push(pkg);
            }
        }
        for (d, base) in &wanted {
            if let Some(Some(dep_base)) = provider.get(d)
                && dep_base != base
            {
                edges
                    .entry(base.clone())
                    .or_default()
                    .insert(dep_base.clone());
            }
        }
        frontier = next;
    }

    let bases: BTreeSet<String> = pkgs.values().map(|p| p.package_base.clone()).collect();
    let order = topo_sort(&bases, &edges)?;
    Ok(Plan {
        pkgs,
        explicit,
        order,
        repo_deps,
    })
}

/// Dependencies first. Errors on cycles.
fn topo_sort(
    bases: &BTreeSet<String>,
    edges: &HashMap<String, BTreeSet<String>>,
) -> Result<Vec<String>> {
    fn visit<'a>(
        b: &'a str,
        edges: &'a HashMap<String, BTreeSet<String>>,
        state: &mut HashMap<&'a str, bool>,
        stack: &mut Vec<&'a str>,
        order: &mut Vec<String>,
    ) -> Result<()> {
        match state.get(b) {
            Some(true) => return Ok(()),
            Some(false) => bail!("dependency cycle: {} -> {b}", stack.join(" -> ")),
            None => {}
        }
        state.insert(b, false);
        stack.push(b);
        for d in edges.get(b).into_iter().flatten() {
            visit(d, edges, state, stack, order)?;
        }
        stack.pop();
        state.insert(b, true);
        order.push(b.to_string());
        Ok(())
    }
    let mut state = HashMap::new();
    let mut order = Vec::new();
    for b in bases {
        visit(b, edges, &mut state, &mut Vec::new(), &mut order)?;
    }
    Ok(order)
}

/// Clones or updates the AUR git repository of a package base.
pub fn fetch(base: &str) -> Result<PathBuf> {
    let dir = clone_dir(base);
    let git = |args: &[&str], cwd: Option<&Path>| -> Result<()> {
        let mut c = Command::new("git");
        if let Some(d) = cwd {
            c.arg("-C").arg(d);
        }
        let st = c
            .args(args)
            .stdin(Stdio::null())
            .status()
            .context("failed to run git")?;
        if !st.success() {
            bail!("git {} failed", args.join(" "));
        }
        Ok(())
    };
    if dir.join(".git").is_dir() {
        git(&["fetch", "-q", "origin", "master"], Some(&dir))?;
        git(&["reset", "-q", "--hard", "FETCH_HEAD"], Some(&dir))?;
    } else {
        std::fs::create_dir_all(dir.parent().unwrap())?;
        let url = format!("{}/{base}.git", crate::rpc::AUR_URL);
        git(&["clone", "-q", &url, &dir.to_string_lossy()], None)?;
    }
    Ok(dir)
}

/// Fetches several bases, at most 4 git connections at a time.
pub fn fetch_all(bases: &[String]) -> Result<()> {
    let errors = Mutex::new(Vec::new());
    for chunk in bases.chunks(4) {
        std::thread::scope(|s| {
            for b in chunk {
                let errors = &errors;
                s.spawn(move || {
                    if let Err(e) = fetch(b) {
                        errors.lock().unwrap().push(format!("{b}: {e:#}"));
                    }
                });
            }
        });
    }
    let errors = errors.into_inner().unwrap();
    if !errors.is_empty() {
        bail!("failed to download:\n  {}", errors.join("\n  "));
    }
    Ok(())
}

fn import_pgp_keys(bases: &[String]) -> Result<()> {
    let mut missing = BTreeSet::new();
    for b in bases {
        let Ok(s) = std::fs::read_to_string(clone_dir(b).join(".SRCINFO")) else {
            continue;
        };
        for key in s
            .lines()
            .filter_map(|l| l.trim().strip_prefix("validpgpkeys = "))
        {
            let known = Command::new("gpg")
                .args(["--list-keys", key])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            if !known {
                missing.insert(key.to_string());
            }
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    msg("PGP keys needed to verify sources:");
    for k in &missing {
        println!("    {k}");
    }
    if util::ask("Import them with gpg --recv-keys?", true)? {
        let ok = Command::new("gpg")
            .arg("--recv-keys")
            .args(&missing)
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            util::warn("importing some keys failed; makepkg may refuse to verify the sources");
        }
    }
    Ok(())
}

/// "foo-bar-1:2.0-1-x86_64.pkg.tar.zst" -> "foo-bar"
fn pkgname_of(path: &Path) -> Option<String> {
    let file = path.file_name()?.to_str()?;
    file.rsplitn(4, '-').nth(3).map(str::to_string)
}

fn build_and_install(
    base: &str,
    plan: &Plan,
    installed: &HashMap<String, String>,
    opts: &Opts,
) -> Result<()> {
    let dir = clone_dir(base);
    msg(&format!("Building {base}"));
    let mut cmd = Command::new("makepkg");
    cmd.current_dir(&dir).args(["--syncdeps", "--force"]);
    if opts.clean {
        cmd.arg("--cleanbuild");
    }
    if opts.nocheck {
        cmd.arg("--nocheck");
    }
    if util::noconfirm() {
        cmd.arg("--noconfirm");
    }
    if !cmd.status().context("failed to run makepkg")?.success() {
        bail!("makepkg failed for {base}");
    }

    let out = Command::new("makepkg")
        .current_dir(&dir)
        .arg("--packagelist")
        .output()?;
    let files: Vec<PathBuf> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(PathBuf::from)
        .collect();

    let (mut as_deps, mut as_default) = (Vec::new(), Vec::new());
    for p in plan.pkgs.values().filter(|p| p.package_base == base) {
        let file = files
            .iter()
            .find(|f| pkgname_of(f).as_deref() == Some(p.name.as_str()))
            .filter(|f| f.exists())
            .with_context(|| format!("makepkg did not produce a package file for {}", p.name))?;
        let is_dep = opts.asdeps || !plan.explicit.contains(&p.name);
        if is_dep && !installed.contains_key(&p.name) {
            as_deps.push(file.to_string_lossy().into_owned());
        } else {
            as_default.push(file.to_string_lossy().into_owned());
        }
    }
    for (files, deps) in [(as_deps, true), (as_default, false)] {
        if files.is_empty() {
            continue;
        }
        let mut args = vec!["-U".to_string()];
        if deps {
            args.push("--asdeps".into());
        }
        if util::noconfirm() {
            args.push("--noconfirm".into());
        }
        args.extend(files);
        if pacman::run(&args, true)? != 0 {
            bail!("pacman failed to install the packages built from {base}");
        }
    }
    Ok(())
}

/// Installs AUR packages (exact pkgnames) and their dependencies.
pub fn install(client: &mut Client, targets: &[String], opts: &Opts) -> Result<i32> {
    if targets.is_empty() {
        return Ok(0);
    }
    if pacman::is_root() {
        bail!(
            "don't run aurgus as root: makepkg refuses to build as root, aurgus uses sudo when needed"
        );
    }
    let installed = pacman::installed()?;
    let info = client.info(targets)?;
    let missing: Vec<&str> = targets
        .iter()
        .filter(|t| !info.contains_key(*t))
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        bail!("target not found: {}", missing.join(", "));
    }

    let mut roots = Vec::new();
    for t in targets {
        let p = &info[t];
        if opts.needed
            && let Some(v) = installed.get(t)
            && vercmp(v, &p.version) != Ordering::Less
        {
            util::warn(&format!("{t}-{v} is up to date -- skipping"));
            continue;
        }
        if !roots.iter().any(|r: &AurPkg| r.name == p.name) {
            roots.push(p.clone());
        }
    }
    if roots.is_empty() {
        println!(" there is nothing to do");
        return Ok(0);
    }

    let plan = resolve(client, roots, opts)?;

    println!();
    msg(&format!("AUR packages ({}):", plan.pkgs.len()));
    for base in &plan.order {
        for p in plan.pkgs.values().filter(|p| &p.package_base == base) {
            let from = installed
                .get(&p.name)
                .map(|v| format!("{v} -> "))
                .unwrap_or_default();
            let tag = if plan.explicit.contains(&p.name) {
                String::new()
            } else {
                dim(" [dep]")
            };
            println!(
                "    {} {}{}{tag}{}",
                bold(&p.name),
                dim(from),
                green(&p.version),
                signals::inline(p)
            );
        }
    }
    if !plan.repo_deps.is_empty() {
        let list: Vec<&str> = plan.repo_deps.iter().map(String::as_str).collect();
        msg(&format!("Repository dependencies: {}", list.join(" ")));
    }
    println!();
    if !util::ask("Proceed with the review?", true)? {
        return Ok(1);
    }

    msg("Downloading PKGBUILDs");
    fetch_all(&plan.order)?;

    for base in &plan.order {
        let pkg = plan
            .pkgs
            .values()
            .find(|p| &p.package_base == base)
            .unwrap();
        if !review::review(base, pkg, opts.review)? {
            util::error(&format!("{base} was not approved; nothing was built"));
            return Ok(1);
        }
    }

    import_pgp_keys(&plan.order)?;

    if !plan.repo_deps.is_empty() {
        msg("Installing repository dependencies");
        let mut args: Vec<String> = ["-S", "--asdeps", "--needed"].map(String::from).into();
        if util::noconfirm() {
            args.push("--noconfirm".into());
        }
        args.extend(plan.repo_deps.iter().cloned());
        if pacman::run(&args, true)? != 0 {
            bail!("pacman failed to install the repository dependencies");
        }
    }

    for base in &plan.order {
        build_and_install(base, &plan, &installed, opts)?;
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(dep_name("foo>=1.2"), "foo");
        assert_eq!(dep_name("bar"), "bar");
        assert_eq!(
            pkgname_of(Path::new("/x/foo-bar-1:2.0-1-x86_64.pkg.tar.zst")).as_deref(),
            Some("foo-bar")
        );
    }

    #[test]
    fn topo() {
        let bases: BTreeSet<String> = ["a", "b", "c"].map(String::from).into();
        let mut edges = HashMap::new();
        edges.insert("a".to_string(), BTreeSet::from(["b".to_string()]));
        edges.insert("b".to_string(), BTreeSet::from(["c".to_string()]));
        assert_eq!(topo_sort(&bases, &edges).unwrap(), ["c", "b", "a"]);
        edges.insert("c".to_string(), BTreeSet::from(["a".to_string()]));
        assert!(topo_sort(&bases, &edges).is_err());
    }
}
