# Session-only Bash completion. Never execute a candidate.
__shiguang_complete_local_command() {
    COMPREPLY=()
    local prefix="${COMP_WORDS[COMP_CWORD]}" candidate
    # Existing commands, aliases and explicit paths keep their original handling.
    if [[ -z "$prefix" || "$prefix" == */* || -n "$(builtin compgen -c -- "$prefix")" ]]; then
        if [[ -n "${__shiguang_previous_initial_function-}" ]] && builtin declare -F "$__shiguang_previous_initial_function" >/dev/null; then
            "$__shiguang_previous_initial_function" "$@"
            return $?
        fi
        return 0
    fi
    while IFS= read -r candidate; do
        [[ -e "./$candidate" || -L "./$candidate" ]] || continue
        COMPREPLY+=("./$candidate")
    done < <(builtin compgen -f -- "$prefix")
    if (( ${#COMPREPLY[@]} == 0 )) && [[ -n "${__shiguang_previous_initial_function-}" ]] && builtin declare -F "$__shiguang_previous_initial_function" >/dev/null; then
        "$__shiguang_previous_initial_function" "$@"
        return $?
    fi
}

# Bash 4 has no complete -I. A readline macro first prepares a local path,
# then invokes native completion through a private key. These bindings are only
# evaluated by Bash readline; full-screen applications still receive raw Tab.
__shiguang_prepare_local_command() {
    local LC_ALL=C before after pattern leading prefix
    builtin bind -m "$__shiguang_readline_map" '"\e[999;98~": '"$__shiguang_native_tab_action" 2>/dev/null || return 0
    before="${READLINE_LINE:0:READLINE_POINT}"
    after="${READLINE_LINE:READLINE_POINT}"
    [[ -z "$after" || "$after" == [[:blank:]]* ]] || return 0
    pattern='^([[:blank:]]*)([[:alnum:]_.-]+|\./[[:alnum:]_.-]*)$'
    [[ "$before" =~ $pattern ]] || return 0
    leading="${BASH_REMATCH[1]}"
    prefix="${BASH_REMATCH[2]}"
    if [[ "$prefix" != ./* ]]; then
        [[ -z "$(builtin compgen -c -- "$prefix")" ]] || return 0
        [[ -n "$(builtin compgen -f -- "$prefix")" ]] || return 0
        READLINE_LINE="${leading}./${READLINE_LINE:${#leading}}"
        READLINE_POINT=$((READLINE_POINT + 2))
    fi
    builtin bind -m "$__shiguang_readline_map" '"\e[999;98~": complete-filename' 2>/dev/null || :
}

__shiguang_setup_readline_completion() {
    local line action
    __shiguang_completion_status=custom
    __shiguang_readline_map=emacs-standard
    [[ -o vi ]] && __shiguang_readline_map=vi-insert
    # Do not overwrite a user's arbitrary shell macro or bind -x command.
    while IFS= read -r line; do
        if [[ "$line" == '"\C-i": '* ]]; then
            [[ "$line" == *'\e[999;97~\e[999;98~'* ]] && __shiguang_completion_status=readline
            return 0
        fi
    done < <(builtin bind -m "$__shiguang_readline_map" -s 2>/dev/null; builtin bind -m "$__shiguang_readline_map" -X 2>/dev/null)
    while IFS= read -r line; do
        if [[ "$line" == '"\C-i": '* ]]; then action="${line#*: }"; break; fi
    done < <(builtin bind -m "$__shiguang_readline_map" -p 2>/dev/null)
    case "$action" in
        complete|menu-complete|menu-complete-backward|possible-completions|complete-filename) ;;
        *) return 0 ;;
    esac
    __shiguang_native_tab_action="$action"
    if builtin bind -m "$__shiguang_readline_map" -x '"\e[999;97~": __shiguang_prepare_local_command' 2>/dev/null &&
       builtin bind -m "$__shiguang_readline_map" '"\e[999;98~": '"$action" 2>/dev/null &&
       builtin bind -m "$__shiguang_readline_map" '"\C-i": "\e[999;97~\e[999;98~"' 2>/dev/null; then
        __shiguang_completion_status=readline
    else
        __shiguang_completion_status=error
    fi
}

__shiguang_setup_completion() {
    local token="$1" last_history previous function_pattern
    # Remove only our own uniquely marked bootstrap line, never a user's entry.
    last_history="$(builtin history 1)"
    if [[ -n "$token" && "$last_history" == *"$token"* ]]; then
        if [[ "$last_history" =~ ^[[:space:]]*([0-9]+)[[:space:]] ]]; then
            builtin history -d "${BASH_REMATCH[1]}" 2>/dev/null || :
        fi
    fi
    __shiguang_completion_status=unsupported
    (( BASH_VERSINFO[0] >= 4 )) || return 0
    if (( BASH_VERSINFO[0] < 5 )); then
        __shiguang_setup_readline_completion
        return 0
    fi
    __shiguang_completion_status=initial
    previous="$(builtin complete -p -I 2>/dev/null)"
    [[ "$previous" == *'-F __shiguang_complete_local_command'* ]] && return 0
    __shiguang_previous_initial_function=''
    if [[ -n "$previous" ]]; then
        # Chain conventional function-based completion (including bash-completion's
        # initial-word loader). Preserve unusual command/string-based rules intact.
        function_pattern='(^|[[:space:]])-F[[:space:]]+([[:alnum:]_:-]+)($|[[:space:]])'
        if ! [[ "$previous" =~ $function_pattern ]]; then __shiguang_completion_status=custom; return 0; fi
        __shiguang_previous_initial_function="${BASH_REMATCH[2]}"
    fi
    builtin shopt -s progcomp
    builtin complete -I -o bashdefault -o default -o filenames -F __shiguang_complete_local_command || __shiguang_completion_status=error
}
