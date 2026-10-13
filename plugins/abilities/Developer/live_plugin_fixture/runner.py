import json,sys
r=json.load(sys.stdin)
print(json.dumps({'protocol':1,'invocation_id':r['invocation_id'],'status':'success','output':{'marker':'IMPLEMENTATION_ALPHA','text':r['arguments']['text']}}))
