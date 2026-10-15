#!/usr/bin/env python3
"""Server operator client for the same protected Code Index maintenance handler."""
import argparse,importlib.util,json,sys,stat,os
from pathlib import Path
def main():
 """Validate structured operator input and call the existing authenticated broker transport."""
 parser=argparse.ArgumentParser(description=__doc__)
 parser.add_argument("action",choices=["list","get","sync","claim","record","release"])
 parser.add_argument("--input",type=Path,help="Bounded JSON object containing action arguments; no shell-embedded JSON required")
 parser.add_argument("--task-id");parser.add_argument("--task-key");parser.add_argument("--scope")
 parser.add_argument("--prepared-job-id");parser.add_argument("--entry-id");parser.add_argument("--status");parser.add_argument("--feature")
 parser.add_argument("--limit",type=int);parser.add_argument("--offset",type=int)
 options=parser.parse_args();args={}
 if options.input:
  if options.input.is_symlink()or not stat.S_ISREG(options.input.stat().st_mode)or options.input.stat().st_nlink!=1 or options.input.stat().st_size>32768:raise ValueError("Bounded regular JSON input required")
  fd=os.open(options.input,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK)
  with os.fdopen(fd,"rb")as stream:
   if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):raise ValueError("Regular JSON input required")
   content=stream.read(32769)
  if len(content)>32768:raise ValueError("JSON input exceeds bound")
  args=json.loads(content)
  if not isinstance(args,dict):raise ValueError("Maintenance input must be an object")
  if "action"in args and args["action"]!=options.action:raise ValueError("Input action does not match command")
 for key in ["task_id","task_key","scope","entry_id","status","feature","limit","offset","prepared_job_id"]:
  value=getattr(options,key)
  if value is not None:
   if key in args and args[key]!=value:raise ValueError("Conflicting CLI/input field: "+key)
   args[key]=value
 args["action"]=options.action;args.setdefault("root","live")
 path=Path(__file__).resolve().parents[4]/"build-service/buildctl.py"
 spec=importlib.util.spec_from_file_location("code_index_operator_transport",path)
 client=importlib.util.module_from_spec(spec);spec.loader.exec_module(client)
 print(json.dumps(client.call("code_index_maintenance",args),ensure_ascii=False))
if __name__=="__main__":
 try:main()
 except(Exception)as exc:print(str(exc),file=sys.stderr);sys.exit(1)
