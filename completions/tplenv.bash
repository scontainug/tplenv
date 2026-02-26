# bash completion for tplenv
_tplenv() {
    local cur prev
    COMPREPLY=()
    cur="${COMP_WORDS[COMP_CWORD]}"
    prev="${COMP_WORDS[COMP_CWORD-1]}"

    local opts="-f --file --file-pattern --values-file --values -o --output -v --verbose --create-values-file --force --value-file-only --eval --indent --context -h --help -V --version"

    case "$prev" in
        -f|--file|--values-file|--values|-o|--output)
            COMPREPLY=( $(compgen -f -- "$cur") )
            return 0
            ;;
        --file-pattern)
            COMPREPLY=( $(compgen -f -- "$cur") )
            return 0
            ;;
    esac

    COMPREPLY=( $(compgen -W "$opts" -- "$cur") )
    return 0
}

complete -F _tplenv tplenv
