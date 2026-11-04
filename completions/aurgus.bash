# bash completion for aurgus
# install: /usr/share/bash-completion/completions/aurgus

_aurgus() {
    local cur prev op words i
    COMPREPLY=()
    cur="${COMP_WORDS[COMP_CWORD]}"
    prev="${COMP_WORDS[COMP_CWORD-1]}"

    # Operation letter from the first -X argument.
    op=""
    for ((i = 1; i < COMP_CWORD; i++)); do
        words="${COMP_WORDS[i]}"
        if [[ $words == -[A-Z]* ]]; then
            op="${words:1:1}"
            break
        fi
        case $words in
            --sync) op=S ;; --query) op=Q ;; --remove) op=R ;;
            --upgrade) op=U ;; --getpkgbuild) op=G ;; --show) op=P ;;
        esac
    done

    case $prev in
        --ignore|--ignoregroup|--config|--root|--dbpath|--cachedir|--arch|--color|--print-format|--overwrite|--assume-installed)
            case $prev in
                --config|--root|--dbpath|--cachedir) _filedir ;;
                --ignore) COMPREPLY=($(compgen -W "$(pacman -Qq 2>/dev/null)" -- "$cur")) ;;
                --color) COMPREPLY=($(compgen -W "always never auto" -- "$cur")) ;;
            esac
            return ;;
    esac

    if [[ $cur == --* ]]; then
        local common="--aur --repo --review --noreview --clean --noclean --check --nocheck --devel --nodevel --gendb --gen-config --help --noconfirm --confirm --needed --asdeps --asexplicit --ignore --ignoregroup --config --root --dbpath --cachedir --color --verbose --debug --quiet"
        case $op in
            S) COMPREPLY=($(compgen -W "$common --sysupgrade --refresh --search --info --list --clean --groups --print --downloadonly --overwrite" -- "$cur")) ;;
            Q) COMPREPLY=($(compgen -W "$common --upgrades --foreign --explicit --deps --info --list --search --file --owns --unrequired --native --changelog --check" -- "$cur")) ;;
            R) COMPREPLY=($(compgen -W "$common --recursive --nosave --unneeded --cascade --nodeps --print" -- "$cur")) ;;
            *) COMPREPLY=($(compgen -W "$common --sync --query --remove --upgrade --database --files --deptest --version --getpkgbuild --show" -- "$cur")) ;;
        esac
        return
    fi

    if [[ $cur == -* ]]; then
        case $op in
            S) COMPREPLY=($(compgen -W "-Ss -Si -Sy -Su -Syu -Syyu -Sc -Scc -Sl -Sg -Sp -Sw" -- "$cur")) ;;
            Q) COMPREPLY=($(compgen -W "-Qu -Qua -Qm -Qe -Qd -Qi -Ql -Qs -Qo -Qdt -Qn -Qq" -- "$cur")) ;;
            R) COMPREPLY=($(compgen -W "-Rns -Rs -Rn -Rc -Rdd" -- "$cur")) ;;
            *) COMPREPLY=($(compgen -W "-S -Ss -Si -Syu -Sc -Q -Qu -Qua -R -Rns -U -G -Ps -F -D -T -V -h" -- "$cur")) ;;
        esac
        return
    fi

    case $op in
        R|Q|D|T)
            COMPREPLY=($(compgen -W "$(pacman -Qq 2>/dev/null)" -- "$cur")) ;;
        U)
            _filedir 'pkg.tar.*' ;;
        P|"")
            if [[ -z $op && $COMP_CWORD -eq 1 ]]; then
                COMPREPLY=($(compgen -W "audit $(aurgus -Pc "$cur" 2>/dev/null)" -- "$cur"))
            elif [[ ${COMP_WORDS[1]} == audit ]]; then
                _filedir
                COMPREPLY+=($(aurgus -Pc "$cur" 2>/dev/null))
            else
                COMPREPLY=($(aurgus -Pc "$cur" 2>/dev/null))
            fi ;;
        *)
            # -S, -G and friends: repos + AUR (only once the prefix has a letter,
            # the full list is ~130k names)
            [[ -n $cur ]] && COMPREPLY=($(aurgus -Pc "$cur" 2>/dev/null)) ;;
    esac
}

complete -F _aurgus aurgus
