#!/usr/bin/env bash
# Gancho PreToolUse de Claude Code: antes de un `git push`, tools/check-locks.sh. Si algún lock no
# está al día, bloquea el push (código 2) y le dice a Claude qué arreglar.
input="$(cat)"
command="$(printf '%s' "$input" | python3 -c 'import json, sys; print(json.load(sys.stdin).get("tool_input", {}).get("command", ""))')"
case "$command" in
    *"git push"*|*"git -c"*" push"*) ;;
    *) exit 0 ;;
esac
root="$(git -C "${CLAUDE_PROJECT_DIR:-.}" rev-parse --show-toplevel 2>/dev/null)" || exit 0
if ! output="$("$root/tools/check-locks.sh" 2>&1)"; then
    printf 'Push bloqueado: hay locks sin actualizar.\n%s\nEjecuta tools/check-locks.sh --fix, haz commit del lock y vuelve a subir.\n' "$output" >&2
    exit 2
fi
exit 0
