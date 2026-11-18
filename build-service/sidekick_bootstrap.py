#!/usr/bin/python3 -I
"""Fixed SSH forced command of the broker sidekick; the only part kept up to date by hand.

Installed root-owned at /usr/local/lib/broker-sidekick/sidekick_bootstrap.py and named by
the broker key's authorized_keys line:

    restrict,from="<server>",command="/usr/bin/python3 -I /usr/local/lib/broker-sidekick/sidekick_bootstrap.py serve" ssh-ed25519 ...

`update SHA256` (stdin: the broker's sidekick.py) stores a new sidekick program after its
hash and syntax check and makes it current, keeping the previous ones for rollback. Every
other verb, and `prune` from the timer, runs the current program (or the root-installed
copy beside this file until the broker has pushed one).
"""
import hashlib,os,re,sys
from pathlib import Path

CODE=Path(os.environ.get("BROKER_SIDEKICK_STATE","/var/lib/broker-sidekick"))/"code"
FALLBACK=Path(__file__).with_name("sidekick.py")
KEEP=3


def current():
    try:sha=(CODE/"current").read_text().strip()
    except OSError:return FALLBACK
    path=CODE/(sha+".py")
    if not re.fullmatch(r"[a-f0-9]{64}",sha)or not path.is_file()or hashlib.sha256(path.read_bytes()).hexdigest()!=sha:return FALLBACK
    return path


def update(sha):
    if not re.fullmatch(r"[a-f0-9]{64}",sha):sys.exit("update: sha256 required")
    body=sys.stdin.buffer.read(2*1024*1024+1)
    if len(body)>2*1024*1024 or hashlib.sha256(body).hexdigest()!=sha:sys.exit("update: content does not match its sha256")
    compile(body,"sidekick.py","exec")
    CODE.mkdir(parents=True,exist_ok=True)
    target=CODE/(sha+".py");temporary=CODE/(sha+".new");temporary.write_bytes(body);os.replace(temporary,target)
    pointer=CODE/"current.new";pointer.write_text(sha);os.replace(pointer,CODE/"current")
    for old in sorted(CODE.glob("*.py"),key=lambda p:p.stat().st_mtime,reverse=True)[KEEP:]:old.unlink()
    print('{"updated": "%s"}'%sha)


def main():
    words=os.environ.get("SSH_ORIGINAL_COMMAND","").split()
    if sys.argv[1:]==["serve"]and len(words)==2 and words[0]=="update":update(words[1]);return
    if sys.argv[1:]not in (["serve"],["prune"]):sys.exit("usage: sidekick_bootstrap.py serve|prune")
    program=current()
    os.execv(sys.executable,[sys.executable,"-I",str(program),*sys.argv[1:]])


if __name__=="__main__":main()
