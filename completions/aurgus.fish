# fish completion for aurgus
# install: /usr/share/fish/vendor_completions.d/aurgus.fish

function __aurgus_op
    set -l tokens (commandline -opc)
    for t in $tokens[2..-1]
        if string match -qr '^-[A-Z]' -- $t
            echo (string sub -s 2 -l 1 -- $t)
            return
        end
        switch $t
            case --sync; echo S; return
            case --query; echo Q; return
            case --remove; echo R; return
            case --upgrade; echo U; return
            case --getpkgbuild; echo G; return
            case --show; echo P; return
        end
    end
end

function __aurgus_packages
    set -l cur (commandline -ct)
    # only once the prefix has a letter: the full list is ~130k names
    test -n "$cur"; and aurgus -Pc "$cur" 2>/dev/null
end

function __aurgus_installed
    pacman -Qq 2>/dev/null
end

function __aurgus_op_is
    contains -- (__aurgus_op) $argv
end

complete -c aurgus -f

# targets
complete -c aurgus -n '__aurgus_op_is S G ""' -a '(__aurgus_packages)'
complete -c aurgus -n '__aurgus_op_is R Q D T' -a '(__aurgus_installed)'
complete -c aurgus -n '__aurgus_op_is U' -F
complete -c aurgus -n 'test -z (__aurgus_op); and test (count (commandline -opc)) -eq 1' -a audit -d 'audit a package or PKGBUILD directory'

# operations
complete -c aurgus -n 'test -z (__aurgus_op)' -o S -d 'sync: install from the repos and the AUR'
complete -c aurgus -n 'test -z (__aurgus_op)' -o Ss -d 'search repos and AUR'
complete -c aurgus -n 'test -z (__aurgus_op)' -o Si -d 'package information'
complete -c aurgus -n 'test -z (__aurgus_op)' -o Syu -d 'upgrade the system, AUR included'
complete -c aurgus -n 'test -z (__aurgus_op)' -o Sc -d 'clean caches and build directories'
complete -c aurgus -n 'test -z (__aurgus_op)' -o Q -d 'query installed packages'
complete -c aurgus -n 'test -z (__aurgus_op)' -o Qu -d 'list upgrades'
complete -c aurgus -n 'test -z (__aurgus_op)' -o Qua -d 'list AUR upgrades'
complete -c aurgus -n 'test -z (__aurgus_op)' -o R -d 'remove packages'
complete -c aurgus -n 'test -z (__aurgus_op)' -o Rns -d 'remove with dependencies and configs'
complete -c aurgus -n 'test -z (__aurgus_op)' -o U -d 'install package files'
complete -c aurgus -n 'test -z (__aurgus_op)' -o G -d 'clone AUR repositories'
complete -c aurgus -n 'test -z (__aurgus_op)' -o Ps -d 'AUR statistics'
complete -c aurgus -n 'test -z (__aurgus_op)' -o V -d 'version'
complete -c aurgus -n 'test -z (__aurgus_op)' -s h -l help -d 'show help'

# aurgus options
complete -c aurgus -l aur -d 'only act on the AUR'
complete -c aurgus -l repo -d 'only act on the repositories'
complete -c aurgus -l review -d 'ask to review every package'
complete -c aurgus -l noreview -d 'do not ask to review clean packages'
complete -c aurgus -l clean -d 'clean build (makepkg --cleanbuild)'
complete -c aurgus -l noclean -d 'reuse build directories'
complete -c aurgus -l check -d 'run check()'
complete -c aurgus -l nocheck -d 'skip check() and checkdepends'
complete -c aurgus -l devel -d 'check upstream of -git packages'
complete -c aurgus -l nodevel -d 'do not check -git packages'
complete -c aurgus -l gendb -d 'record upstream commits of installed -git packages'
complete -c aurgus -l gen-config -d 'print an example configuration file'

# pacman options
complete -c aurgus -l noconfirm -d 'do not ask for confirmation'
complete -c aurgus -l needed -d 'do not reinstall up-to-date packages'
complete -c aurgus -l asdeps -d 'install as dependencies'
complete -c aurgus -l asexplicit -d 'install as explicitly installed'
complete -c aurgus -l ignore -x -a '(__aurgus_installed)' -d 'ignore a package upgrade'
complete -c aurgus -l ignoregroup -x -d 'ignore a group upgrade'
complete -c aurgus -l config -r -F -d 'alternate pacman.conf'
complete -c aurgus -l root -r -F -d 'alternate installation root'
complete -c aurgus -l dbpath -r -F -d 'alternate database location'
complete -c aurgus -l color -x -a 'always never auto' -d 'colorize output'
complete -c aurgus -n '__aurgus_op_is S' -s y -l refresh -d 'refresh databases and the AUR index'
complete -c aurgus -n '__aurgus_op_is S' -s u -l sysupgrade -d 'upgrade all packages'
complete -c aurgus -n '__aurgus_op_is S' -s s -l search -d 'search'
complete -c aurgus -n '__aurgus_op_is S' -s i -l info -d 'package information'
complete -c aurgus -n '__aurgus_op_is S' -s w -l downloadonly -d 'download only'
complete -c aurgus -n '__aurgus_op_is Q' -s u -l upgrades -d 'list upgrades'
complete -c aurgus -n '__aurgus_op_is Q' -s a -d 'AUR packages only'
complete -c aurgus -n '__aurgus_op_is Q' -s m -l foreign -d 'foreign packages'
complete -c aurgus -n '__aurgus_op_is Q' -s e -l explicit -d 'explicitly installed'
complete -c aurgus -n '__aurgus_op_is Q' -s d -l deps -d 'installed as dependencies'
complete -c aurgus -n '__aurgus_op_is Q' -s t -l unrequired -d 'not required by others'
complete -c aurgus -n '__aurgus_op_is Q' -s i -l info -d 'package information'
complete -c aurgus -n '__aurgus_op_is Q' -s l -l list -d 'list package files'
complete -c aurgus -n '__aurgus_op_is Q' -s o -l owns -r -F -d 'package owning a file'
complete -c aurgus -n '__aurgus_op_is Q' -s q -l quiet -d 'show less information'
complete -c aurgus -n '__aurgus_op_is R' -s s -l recursive -d 'remove unneeded dependencies'
complete -c aurgus -n '__aurgus_op_is R' -s n -l nosave -d 'remove configuration files'
complete -c aurgus -n '__aurgus_op_is R' -s c -l cascade -d 'remove dependents too'
