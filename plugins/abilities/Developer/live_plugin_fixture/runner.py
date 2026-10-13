"""Pure acceptance echo with a bounded delay for immutable-generation checks."""
import json,sys,pathlib,runpy,time
request=json.load(sys.stdin)
if request["arguments"].get("text")=="PINNED_DELAY":
    time.sleep(60)
marker=runpy.run_path(str(pathlib.Path(__file__).with_name("helper.py")))["MARKER"]
print(json.dumps({"protocol":1,"invocation_id":request["invocation_id"],"status":"success","output":{"marker":marker,"text":request["arguments"]["text"]}},separators=(",",":")))
