# mimi shell integration for fish.
#
# Emits OSC 133 (semantic prompt) and OSC 633 (command line / cwd) markers so
# mimi can turn every command into a structured block: command, cwd, output,
# exit code, timing. Auto-loaded via fish's vendor_conf.d mechanism — mimi
# prepends its share directory to $XDG_DATA_DIRS when spawning fish.

status is-interactive; or exit
test "$TERM_PROGRAM" = mimi; or exit
set -q __mimi_integration_active; and exit
set -g __mimi_integration_active 1

function __mimi_osc133_prompt --on-event fish_prompt
    printf '\e]133;A\a'
end

function __mimi_preexec --on-event fish_preexec
    # Report the command line (633;E, VS Code-style escaping), then mark
    # pre-execution (133;C).
    set -l cmd (string replace -a \\ \\\\ -- $argv[1] | string replace -a \; \\x3b | string join ' ')
    printf '\e]633;E;%s\a' "$cmd"
    printf '\e]133;C\a'
end

function __mimi_postexec --on-event fish_postexec
    printf '\e]133;D;%s\a' $status
end

function __mimi_report_cwd --on-variable PWD
    printf '\e]7;file://%s%s\a' (hostname) "$PWD"
end
__mimi_report_cwd

# Wrap the prompt to mark where user input begins (133;B).
if functions -q fish_prompt; and not functions -q __mimi_original_prompt
    functions -c fish_prompt __mimi_original_prompt
    function fish_prompt
        __mimi_original_prompt
        printf '\e]133;B\a'
    end
end
