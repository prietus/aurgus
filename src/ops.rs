//! Top-level operations (what each pacman-style flag combination does).

use crate::args::Args;
use crate::audit::Severity;
use crate::install::{self, Opts, dep_name};
use crate::pacman;
use crate::review;
use crate::rpc::{AUR_URL, AurPkg, Client, DAILY_SOFT_CAP};
use crate::signals;
use crate::util::{self, bold, clone_dir, cyan, dim, green, magenta, msg, red, yellow};
use crate::{config, devel, meta, vercmp::vercmp};
use anyhow::{Result, bail};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

pub fn dispatch(mut args: Args) -> Result<i32> {
    match args.op {
        None if args.has('h') || args.has_long("help") => {
            help();
            Ok(0)
        }
        None if args.own("gen-config") => {
            print!("{}", config::EXAMPLE);
            Ok(0)
        }
        None if args.own("gendb") => gendb(),
        None if args.targets.is_empty() => {
            // Bare `aurgus` = `aurgus -Syu`, like yay/paru.
            args.op = Some('S');
            args.shorts.extend(['y', 'u']);
            sysupgrade(&args)
        }
        None if args.targets[0] == "audit" => audit_cmd(&args.targets[1..]),
        None => interactive(&args),
        Some('S') => sync(&args),
        Some('Q') => query(&args),
        Some('G') => getpkgbuild(&args),
        Some('P') => show(&args),
        Some('V') => {
            println!("aurgus {}", env!("CARGO_PKG_VERSION"));
            pacman::run(&args.pacman_args(), false)
        }
        Some(op) => {
            let root = matches!(op, 'R' | 'U')
                || (op == 'D' && !args.has('k'))
                || (op == 'F' && args.has('y'));
            passthrough(&args, root)
        }
    }
}

fn passthrough(args: &Args, root: bool) -> Result<i32> {
    let mut v = args.pacman_args();
    v.extend(args.targets.iter().cloned());
    pacman::run(&v, root)
}

/// Packages to skip on upgrade: `--ignore` plus pacman.conf's IgnorePkg.
fn ignored(args: &Args) -> Vec<String> {
    let config = args.longs.iter().find_map(|l| l.strip_prefix("--config="));
    let mut v: Vec<String> = args
        .longs
        .iter()
        .filter_map(|l| l.strip_prefix("--ignore="))
        .flat_map(|v| v.split(',').map(str::to_string))
        .collect();
    v.extend(pacman::conf_ignorepkg(config));
    v
}

/// Splits targets into (repository, AUR). `aur/foo` and `core/foo` prefixes
/// force the side; otherwise anything a sync repo knows goes to pacman.
fn split_targets(args: &Args) -> Result<(Vec<String>, Vec<String>)> {
    if args.own("repo") {
        return Ok((args.targets.clone(), Vec::new()));
    }
    if args.own("aur") {
        let aur = args
            .targets
            .iter()
            .map(|t| t.strip_prefix("aur/").unwrap_or(t).to_string())
            .collect();
        return Ok((Vec::new(), aur));
    }
    let sync = pacman::sync_names()?;
    let groups = pacman::group_names()?;
    let (mut repo, mut aur) = (Vec::new(), Vec::new());
    for t in &args.targets {
        if let Some(n) = t.strip_prefix("aur/") {
            aur.push(n.to_string());
        } else if t.contains('/')
            || sync.contains(dep_name(t))
            || groups.contains(t)
            || pacman::repo_satisfies(t)
        {
            repo.push(t.clone());
        } else {
            aur.push(t.clone());
        }
    }
    Ok((repo, aur))
}

fn sync(args: &Args) -> Result<i32> {
    if args.has('s') {
        return search(args);
    }
    if args.has('i') {
        return info(args);
    }
    if args.has('c') {
        let code = passthrough(args, true)?;
        clean_builds()?;
        return Ok(code);
    }
    if args.has('l') || args.has('g') || args.has('p') {
        return passthrough(args, false);
    }
    if args.has('w') {
        return passthrough(args, true);
    }
    if args.has('y') || args.has('u') {
        return sysupgrade(args);
    }
    if args.targets.is_empty() {
        bail!("no targets specified (use -h for help)");
    }
    let (repo, aur) = split_targets(args)?;
    if !repo.is_empty() {
        let mut v = args.pacman_args();
        v.extend(repo);
        let code = pacman::run(&v, true)?;
        if code != 0 {
            return Ok(code);
        }
    }
    let mut client = Client::new();
    install::install(&mut client, &aur, &Opts::from_args(args))
}

fn clean_builds() -> Result<()> {
    let dir = util::cache_dir().join("clone");
    if dir.exists()
        && util::ask(
            &format!("Remove AUR build directories in {}?", dir.display()),
            true,
        )?
    {
        std::fs::remove_dir_all(&dir)?;
    }
    Ok(())
}

fn sysupgrade(args: &Args) -> Result<i32> {
    let (repo, aur_targets) = if args.targets.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        split_targets(args)?
    };
    if !args.own("aur") {
        let mut v = args.pacman_args();
        v.extend(repo);
        let code = pacman::run(&v, true)?;
        if code != 0 {
            return Ok(code);
        }
    }
    if args.own("repo") {
        return Ok(0);
    }
    let mut client = Client::new();
    if args.has('y') {
        match meta::refresh(&mut client, args.count('y') > 1) {
            Ok(true) => msg("AUR package index updated"),
            Ok(false) => {}
            Err(e) => util::warn(&format!("could not refresh the AUR package index: {e:#}")),
        }
    }
    let mut targets = aur_targets;
    if args.has('u') {
        msg("Searching the AUR for updates...");
        let (updates, _) = aur_updates(&mut client, &ignored(args))?;
        if updates.is_empty() && targets.is_empty() {
            println!(" AUR packages are up to date");
        }
        for (name, _, _) in updates {
            if !targets.contains(&name) {
                targets.push(name);
            }
        }
        if args.flag("devel", config::get().devel) {
            msg("Checking upstream of -git packages...");
            for name in devel_updates(&mut client, &ignored(args))? {
                if !targets.contains(&name) {
                    targets.push(name);
                }
            }
        }
    }
    install::install(&mut client, &targets, &Opts::from_args(args))
}

/// Installed `-git` packages with new upstream commits (names).
fn devel_updates(client: &mut Client, ignore: &[String]) -> Result<Vec<String>> {
    let foreign: HashMap<String, String> = pacman::foreign()?
        .into_iter()
        .filter(|(n, _)| devel::is_vcs(n) && !pacman::is_ignored(n, ignore))
        .collect();
    let names: Vec<String> = foreign.keys().cloned().collect();
    let bases: HashMap<String, String> = client
        .info(&names)?
        .into_iter()
        .map(|(n, p)| (n, p.package_base))
        .collect();
    devel::updates(&foreign, &bases)
}

fn gendb() -> Result<i32> {
    let foreign: HashMap<String, String> = pacman::foreign()?
        .into_iter()
        .filter(|(n, _)| devel::is_vcs(n))
        .collect();
    let names: Vec<String> = foreign.keys().cloned().collect();
    let mut client = Client::new();
    let bases: HashMap<String, String> = client
        .info(&names)?
        .into_iter()
        .map(|(n, p)| (n, p.package_base))
        .collect();
    let n = devel::gendb(&foreign, &bases)?;
    msg(&format!(
        "Recorded upstream commits of {n} -git package base(s)"
    ));
    Ok(0)
}

/// (name, installed, available) for foreign packages with a newer AUR
/// version, plus the foreign packages that are not in the AUR at all.
fn aur_updates(
    client: &mut Client,
    ignore: &[String],
) -> Result<(Vec<(String, String, String)>, Vec<String>)> {
    let foreign = pacman::foreign()?;
    let names: Vec<String> = foreign.iter().map(|(n, _)| n.clone()).collect();
    let info = client.info(&names)?;
    let (mut updates, mut gone) = (Vec::new(), Vec::new());
    for (name, local) in foreign {
        match info.get(&name) {
            Some(p) if vercmp(&p.version, &local) == Ordering::Greater => {
                if pacman::is_ignored(&name, ignore) {
                    util::warn(&format!(
                        "{name}: ignoring package upgrade ({local} => {})",
                        p.version
                    ));
                } else {
                    updates.push((name, local, p.version.clone()));
                }
            }
            Some(_) => {}
            None => gone.push(name),
        }
    }
    Ok((updates, gone))
}

fn query(args: &Args) -> Result<i32> {
    if !args.has('u') {
        return passthrough(args, false);
    }
    let mut any = false;
    if !args.has('a') {
        let mut v = args.pacman_args_without(&['a']);
        v.extend(args.targets.iter().cloned());
        any |= pacman::run(&v, false)? == 0;
    }
    if !args.has('n') {
        let mut client = Client::new();
        let (updates, _) = aur_updates(&mut client, &ignored(args))?;
        for (name, old, new) in &updates {
            if args.has('q') {
                println!("{name}");
            } else {
                println!("{} {} -> {}", bold(name), red(old), green(new));
            }
        }
        any |= !updates.is_empty();
        if args.has('a') && args.flag("devel", config::get().devel) {
            let dev = devel_updates(&mut client, &ignored(args))?;
            for name in &dev {
                if args.has('q') {
                    println!("{name}");
                } else {
                    println!("{} {}", bold(name), dim("(new upstream commits)"));
                }
            }
            any |= !dev.is_empty();
        }
    }
    Ok(if any { 0 } else { 1 })
}

/// AUR search: from the local index when it's fresh (no RPC at all),
/// otherwise one RPC request on the longest term, filtered by the rest.
fn aur_search(client: &mut Client, terms: &[String]) -> Result<Vec<AurPkg>> {
    let lower: Vec<String> = terms.iter().map(|t| t.to_lowercase()).collect();
    let matches = |p: &AurPkg| {
        let name = p.name.to_lowercase();
        let desc = p.description.as_deref().unwrap_or("").to_lowercase();
        lower.iter().all(|t| name.contains(t) || desc.contains(t))
    };
    let local = match meta::age() {
        Some(age) if age < meta::MAX_AGE_FOR_SEARCH => meta::search(&lower)?,
        _ => None,
    };
    let mut res: Vec<AurPkg> = match local {
        Some(hits) => hits,
        None => {
            let longest = terms.iter().max_by_key(|t| t.len()).unwrap();
            client
                .search(longest, "name-desc")?
                .into_iter()
                .filter(|p| matches(p))
                .collect()
        }
    };
    res.sort_by(|a, b| {
        b.popularity
            .total_cmp(&a.popularity)
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(res)
}

fn print_aur_entry(p: &AurPkg, installed: &HashMap<String, String>, num: Option<usize>) {
    let mut line = String::new();
    if let Some(n) = num {
        line.push_str(&format!("{} ", magenta(n.to_string())));
    }
    line.push_str(&format!(
        "{}/{} {}",
        cyan("aur"),
        bold(&p.name),
        green(&p.version)
    ));
    line.push_str(&dim(format!(" (+{} {:.2})", p.num_votes, p.popularity)));
    if p.maintainer.is_none() {
        line.push_str(&red(" [orphaned]"));
    }
    if p.out_of_date.is_some() {
        line.push_str(&red(" [out-of-date]"));
    }
    match installed.get(&p.name) {
        Some(v) if *v == p.version => line.push_str(&cyan(" [installed]")),
        Some(v) => line.push_str(&cyan(format!(" [installed: {v}]"))),
        None => {}
    }
    println!("{line}");
    println!("    {}", p.description.as_deref().unwrap_or(""));
}

fn search(args: &Args) -> Result<i32> {
    if args.targets.is_empty() {
        return passthrough(args, false);
    }
    let mut found = false;
    if !args.own("aur") {
        let mut v = args.pacman_args();
        v.push("--".into());
        v.extend(args.targets.iter().cloned());
        found |= pacman::run(&v, false)? == 0;
    }
    if !args.own("repo") {
        let installed = pacman::installed()?;
        let mut client = Client::new();
        let res = aur_search(&mut client, &args.targets)?;
        for p in &res {
            if args.has('q') {
                println!("{}", p.name);
            } else {
                print_aur_entry(p, &installed, None);
            }
        }
        found |= !res.is_empty();
    }
    Ok(if found { 0 } else { 1 })
}

fn info(args: &Args) -> Result<i32> {
    if args.targets.is_empty() {
        return passthrough(args, false);
    }
    let (repo, aur) = split_targets(args)?;
    let mut code = 0;
    if !repo.is_empty() {
        let mut v = args.pacman_args();
        v.extend(repo);
        code = pacman::run(&v, false)?;
    }
    if !aur.is_empty() {
        let mut client = Client::new();
        let found = client.info(&aur)?;
        for t in &aur {
            match found.get(t) {
                Some(p) => print_aur_info(p),
                None => {
                    util::error(&format!("package '{t}' was not found"));
                    code = 1;
                }
            }
        }
    }
    Ok(code)
}

fn print_aur_info(p: &AurPkg) {
    let row = |k: &str, v: String| println!("{} : {v}", bold(format!("{k:<15}")));
    let list = |v: &[String]| {
        if v.is_empty() {
            "None".to_string()
        } else {
            v.join("  ")
        }
    };
    row("Repository", "aur".into());
    row("Name", p.name.clone());
    row("Package Base", p.package_base.clone());
    row("Version", p.version.clone());
    row("Description", p.description.clone().unwrap_or_default());
    row("URL", p.url.clone().unwrap_or_else(|| "None".into()));
    row("AUR URL", format!("{AUR_URL}/packages/{}", p.name));
    row("Licenses", list(&p.license));
    row("Keywords", list(&p.keywords));
    row("Provides", list(&p.provides));
    row("Depends On", list(&p.depends));
    row("Make Deps", list(&p.make_depends));
    row("Check Deps", list(&p.check_depends));
    row("Optional Deps", list(&p.opt_depends));
    row("Conflicts With", list(&p.conflicts));
    row("Replaces", list(&p.replaces));
    row(
        "Maintainer",
        p.maintainer
            .clone()
            .unwrap_or_else(|| red("None (orphaned)")),
    );
    row("Co-Maintainers", list(&p.co_maintainers));
    row("Votes", p.num_votes.to_string());
    row("Popularity", format!("{:.4}", p.popularity));
    row("First Submitted", util::fmt_date(p.first_submitted));
    row("Last Modified", util::fmt_date(p.last_modified));
    row(
        "Out-of-date",
        p.out_of_date
            .map(|t| red(format!("Yes (since {})", util::fmt_date(t))))
            .unwrap_or_else(|| "No".into()),
    );
    let tags = signals::tags(p);
    row(
        "Review Hints",
        if tags.is_empty() {
            "None".into()
        } else {
            tags.join(", ")
        },
    );
    println!();
}

/// `aurgus <terms>`: numbered search, then install the selection (yay-style).
fn interactive(args: &Args) -> Result<i32> {
    enum Hit {
        Repo(pacman::RepoHit),
        Aur(AurPkg),
    }
    let installed = pacman::installed()?;
    let mut client = Client::new();
    let mut hits: Vec<Hit> = Vec::new();
    if !args.own("aur") {
        hits.extend(pacman::search(&args.targets)?.into_iter().map(Hit::Repo));
    }
    if !args.own("repo") {
        hits.extend(
            aur_search(&mut client, &args.targets)?
                .into_iter()
                .map(Hit::Aur),
        );
    }
    if hits.is_empty() {
        util::error("no packages found");
        return Ok(1);
    }
    // Best match is number 1, printed last so it sits next to the prompt.
    for (i, h) in hits.iter().enumerate().rev() {
        match h {
            Hit::Aur(p) => print_aur_entry(p, &installed, Some(i + 1)),
            Hit::Repo(r) => {
                let mark = if installed.contains_key(&r.name) {
                    cyan(" [installed]")
                } else {
                    String::new()
                };
                println!(
                    "{} {}/{} {}{mark}\n    {}",
                    magenta((i + 1).to_string()),
                    yellow(&r.repo),
                    bold(&r.name),
                    green(&r.version),
                    r.desc
                );
            }
        }
    }
    let sel = util::read_line(&format!(
        "{} Packages to install (eg: 1 2 3, 1-3): ",
        green("==>")
    ))?;
    let (mut repo, mut aur) = (Vec::new(), Vec::new());
    for i in parse_selection(&sel, hits.len()) {
        match &hits[i - 1] {
            Hit::Repo(r) => repo.push(format!("{}/{}", r.repo, r.name)),
            Hit::Aur(p) => aur.push(p.name.clone()),
        }
    }
    if repo.is_empty() && aur.is_empty() {
        return Ok(0);
    }
    if !repo.is_empty() {
        let mut v = vec!["-S".to_string()];
        v.extend(args.longs.iter().cloned());
        v.extend(repo);
        let code = pacman::run(&v, true)?;
        if code != 0 {
            return Ok(code);
        }
    }
    install::install(&mut client, &aur, &Opts::from_args(args))
}

/// "1 2 4-6,9" -> [1, 2, 4, 5, 6, 9] (ignores out-of-range numbers).
fn parse_selection(s: &str, max: usize) -> Vec<usize> {
    let mut out = Vec::new();
    for tok in s
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|t| !t.is_empty())
    {
        let (a, b) = match tok.split_once('-') {
            Some((a, b)) => (a.parse::<usize>(), b.parse::<usize>()),
            None => (tok.parse(), tok.parse()),
        };
        if let (Ok(a), Ok(b)) = (a, b) {
            for i in a.min(b)..=a.max(b) {
                if (1..=max).contains(&i) && !out.contains(&i) {
                    out.push(i);
                }
            }
        }
    }
    out
}

fn getpkgbuild(args: &Args) -> Result<i32> {
    if args.targets.is_empty() {
        bail!("no targets specified");
    }
    let mut client = Client::new();
    let found = client.info(&args.targets)?;
    let mut code = 0;
    for t in &args.targets {
        let Some(p) = found.get(t) else {
            util::error(&format!(
                "{t} is not in the AUR (for repo packages use `pkgctl repo clone`)"
            ));
            code = 1;
            continue;
        };
        let base = &p.package_base;
        if Path::new(base).exists() {
            util::warn(&format!("{base}: directory already exists, skipping"));
            continue;
        }
        msg(&format!("Cloning {base}"));
        if !Command::new("git")
            .args(["clone", "-q", &format!("{AUR_URL}/{base}.git")])
            .status()?
            .success()
        {
            code = 1;
        }
    }
    Ok(code)
}

fn audit_cmd(targets: &[String]) -> Result<i32> {
    if targets.is_empty() {
        bail!("usage: aurgus audit <package|directory>...");
    }
    let state = review::load();
    let mut client: Option<Client> = None;
    let mut code = 0;
    for t in targets {
        let path = Path::new(t);
        let (name, report) = if path.join("PKGBUILD").is_file() {
            {
                let name = std::fs::canonicalize(path)
                    .ok()
                    .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .unwrap_or_else(|| t.clone());
                (name, review::analyze(path, None, None)?)
            }
        } else {
            let c = client.get_or_insert_with(Client::new);
            let Some(p) = c.info(std::slice::from_ref(t))?.remove(t) else {
                util::error(&format!(
                    "{t}: not a directory with a PKGBUILD nor an AUR package"
                ));
                code = 1;
                continue;
            };
            install::fetch(&p.package_base)?;
            let report = review::analyze(
                &clone_dir(&p.package_base),
                Some(&p),
                state.get(&p.package_base),
            )?;
            (p.package_base.clone(), report)
        };
        review::print_report(&name, &report);
        if report.new_count(Severity::High) > 0 {
            code = 2;
        }
    }
    Ok(code)
}

fn show(args: &Args) -> Result<i32> {
    if args.has('c') {
        return complete(args.targets.first().map(String::as_str).unwrap_or(""));
    }
    if !args.has('s') {
        bail!("usage: aurgus -Ps (statistics) or -Pc [prefix] (completion)");
    }
    let installed = pacman::installed()?;
    let foreign = pacman::foreign()?;
    let mut client = Client::new();
    let names: Vec<String> = foreign.iter().map(|(n, _)| n.clone()).collect();
    let found = client.info(&names)?;

    let (mut updates, mut orphans, mut outdated, mut gone) = (0, vec![], vec![], vec![]);
    for (n, v) in &foreign {
        match found.get(n) {
            Some(p) => {
                if vercmp(&p.version, v) == Ordering::Greater {
                    updates += 1;
                }
                if p.maintainer.is_none() {
                    orphans.push(n.as_str());
                }
                if p.out_of_date.is_some() {
                    outdated.push(n.as_str());
                }
            }
            None => gone.push(n.as_str()),
        }
    }
    let row = |k: &str, v: String| println!("{} {v}", bold(format!("{k:<28}")));
    let list = |v: &[&str]| {
        if v.is_empty() {
            green("none")
        } else {
            yellow(v.join(" "))
        }
    };
    row("Installed packages:", installed.len().to_string());
    row(
        "Explicitly installed:",
        pacman::explicit_count()?.to_string(),
    );
    row("Foreign (AUR/local):", foreign.len().to_string());
    row("AUR updates available:", updates.to_string());
    row("Orphaned in the AUR:", list(&orphans));
    row("Flagged out-of-date:", list(&outdated));
    row("Not in the AUR:", list(&gone));
    row("Reviewed package bases:", review::load().len().to_string());
    row(
        "Local AUR index:",
        meta::age()
            .map(|a| format!("checked {} min ago", a / 60))
            .unwrap_or_else(|| "none (run aurgus -Sy)".into()),
    );
    row(
        "AUR RPC requests today:",
        format!("{} / {DAILY_SOFT_CAP} (soft cap)", client.requests_today()),
    );
    Ok(0)
}

/// `-Pc [prefix]`: package names from the sync repos and the local AUR index,
/// for shell completion. Never touches the network.
fn complete(prefix: &str) -> Result<i32> {
    let mut names: Vec<String> = pacman::sync_names()?
        .into_iter()
        .filter(|n| n.starts_with(prefix))
        .collect();
    names.extend(meta::names_with_prefix(prefix));
    names.sort_unstable();
    names.dedup();
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    for n in names {
        use std::io::Write;
        writeln!(out, "{n}")?;
    }
    Ok(0)
}

fn help() {
    println!(
        "aurgus {} — AUR helper that audits PKGBUILDs before building them

usage:  aurgus                      upgrade the system (= -Syu)
        aurgus <terms>              search and pick packages to install
        aurgus <operation> [...]    pacman-compatible operations

AUR-aware operations:
  -S <pkgs>        install from the repos and/or the AUR (aur/foo forces the AUR)
  -Ss <terms>      search repos and AUR (uses the local index when fresh)
  -Si <pkgs>       package info, with review hints (new, few votes, orphan)
  -Sy / -Syu       refresh (also the local AUR index) / upgrade, AUR included
  -Sc              clean pacman's cache and aurgus build directories
  -Qu / -Qua       list updates / only AUR updates (--devel: -git packages too)
  -G <pkgs>        clone the AUR git repositories into the current directory
  -Ps              statistics about installed AUR packages
  -Pc [prefix]     package names for shell completion (repos + local AUR index)
  audit <pkg|dir>  audit an AUR package or a local PKGBUILD without building

aurgus options:
  --aur / --repo   only act on the AUR / the repositories
  --devel          with -Syu/-Qua: check upstream of -git packages (git ls-remote)
  --gendb          record the current upstream commits of installed -git packages
  --noreview       don't ask to review clean packages (high-severity
                   audit findings are always shown and asked about)
  --clean          clean build (makepkg --cleanbuild)
  --nocheck        skip check() and checkdepends
  --gen-config     print an example configuration file

Configuration file: {config}
  Create it with `aurgus --gen-config > file`. Flags on the command line
  always win; negated forms (--nodevel, --review, --noclean, --check)
  override values set in the file.

Anything else is passed to pacman (with sudo when needed).
Set AURGUS_SUDO to use doas/run0 instead of sudo.",
        env!("CARGO_PKG_VERSION"),
        config = config::path().display()
    );
}

#[cfg(test)]
mod tests {
    #[test]
    fn selection() {
        assert_eq!(super::parse_selection("1 3-4,9 2", 5), [1, 3, 4, 2]);
        assert_eq!(super::parse_selection("5-3", 9), [3, 4, 5]);
        assert!(super::parse_selection("x", 3).is_empty());
    }
}
