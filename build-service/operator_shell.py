"""Forced command for restricted operator SSH keys: run only allowed buildctl commands as the broker's operator.

authorized_keys of the release login names the key's identity, for example
  command="sudo -n -u ar /usr/bin/python3 -I /usr/local/lib/webagent-ops/operator_shell.py laptop2",restrict ssh-ed25519 ...
The identity must be listed in build-service.json scheduler.operators. Commands come only from
SSH_ORIGINAL_COMMAND; anything that names a server path, installs the broker or drains it is refused.
"""
import os
import shlex
import sys

BUILDCTL = "/home/ar/webagent-dev3/code/build-service/buildctl.py"
CONFIG = "/etc/webagent/development/build-service.json"
# Subcommands a release key may run; stdin replaces file arguments where a command takes one.
ALLOWED = {"status", "inputs", "begin", "finish", "preflight", "decide", "abandon", "progress", "watch", "monitor",
           "job", "cancel", "wait", "check", "heartbeat", "leftovers", "schedule", "apply"}
STDIN_ONLY = {"schedule": "--input", "apply": "--patch"}


def command(identity, original):
    """Return the buildctl argv for an allowed request, or raise ValueError naming the refusal."""
    if not identity or not identity.replace("-", "").isalnum() or identity != identity.lower():
        raise ValueError("invalid key identity")
    words = shlex.split(original or "")
    if words[:1] == ["buildctl"]:
        words = words[1:]
    if not words or words[0] not in ALLOWED:
        raise ValueError("command not allowed for a release key: " + (words[0] if words else "(none)"))
    option = STDIN_ONLY.get(words[0])
    if option is not None:
        values = [words[i + 1] if i + 1 < len(words) else "" for i, w in enumerate(words) if w == option]
        values += [w.split("=", 1)[1] for w in words if w.startswith(option + "=")]
        if values != ["-"]:
            raise ValueError(option + " must be - (stdin) for a release key")
    return words


def main():
    try:
        words = command(sys.argv[1] if len(sys.argv) == 2 else "", os.environ.get("SSH_ORIGINAL_COMMAND"))
    except ValueError as error:
        print("refused: " + str(error), file=sys.stderr)
        return 2
    environment = {"PATH": "/usr/bin:/bin", "LANG": "C.UTF-8", "WEBAGENT_BUILD_CONFIG": CONFIG,
                   "WEBAGENT_OPERATOR": sys.argv[1], "HOME": os.path.expanduser("~")}
    os.execve("/usr/bin/python3", ["/usr/bin/python3", "-B", BUILDCTL, *words], environment)


if __name__ == "__main__":
    sys.exit(main())
