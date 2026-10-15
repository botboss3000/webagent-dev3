#!/usr/bin/python3 -I
"""Stable root-owned timer entrypoint; resolve only the installed development guard."""
import json,os,re,stat
from pathlib import Path
def main():
    """Never execute checkout code or accept caller-controlled cleanup paths."""
    if os.geteuid()!=0:raise PermissionError("Root guard required")
    config=Path("/etc/webagent/development/build-service.json")
    cursor=config
    while cursor!=cursor.parent:
        s=cursor.lstat()
        if stat.S_ISLNK(s.st_mode)or s.st_uid!=0 or s.st_mode&0o022:raise PermissionError("Protected config path required")
        cursor=cursor.parent
    value=json.loads(config.read_bytes());directory=value.get("service_dir","")
    if not re.fullmatch(r"/var/lib/webagent/development/build-service/releases/[a-f0-9]{64}",directory):raise PermissionError("Fixed development installation required")
    if not value.get("scheduler",{}).get("task_aware_cleanup",False):
        print(json.dumps({"applied":False,"deferred":"Task-aware cleanup is disabled"}));return
    target=Path(directory)/"docker_retention.py"
    if not target.exists()and not target.is_symlink():
        print(json.dumps({"applied":False,"deferred":"Coordinated V3 retention not installed; unknown and required artifacts preserved"}))
        return
    cursor=target
    while cursor!=cursor.parent:
        s=cursor.lstat()
        if stat.S_ISLNK(s.st_mode)or s.st_uid!=0 or s.st_mode&0o022:raise PermissionError("Protected guard path required")
        cursor=cursor.parent
    os.execv("/usr/bin/python3",["python3","-I",str(target),"--apply"])
if __name__=="__main__":main()
