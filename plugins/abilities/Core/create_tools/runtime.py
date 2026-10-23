"""Bounded pure Python expression functions. No eval, exec, imports or ambient objects."""
import ast
import json
import math
import operator

OPS = {ast.Add: operator.add, ast.Sub: operator.sub, ast.Mult: operator.mul,
       ast.Div: operator.truediv, ast.FloorDiv: operator.floordiv, ast.Mod: operator.mod}
COMPARE = {ast.Eq: operator.eq, ast.NotEq: operator.ne, ast.Lt: operator.lt,
           ast.LtE: operator.le, ast.Gt: operator.gt, ast.GtE: operator.ge,
           ast.In: lambda a,b: a in b, ast.NotIn: lambda a,b: a not in b}
CALLS = {"len": len, "sum": sum, "min": min, "max": max, "abs": abs,
         "round": round, "sorted": sorted, "str": str, "int": int,
         "float": float, "bool": bool}


def bounded(value, depth=0, budget=None):
    if budget is None:
        budget = [4096, 32768]
    budget[0] -= 1
    if budget[0] < 0:
        raise ValueError("total value node budget exceeded")
    if depth > 24:
        raise ValueError("value depth exceeded")
    if value is None or type(value) is bool:
        return value
    if type(value) is int and value.bit_length() <= 256:
        return value
    if type(value) is float and math.isfinite(value) and abs(value) <= 1e100:
        return value
    if type(value) is str and len(value) <= 4096:
        budget[1] -= len(value)
        if budget[1] < 0:
            raise ValueError("total value text budget exceeded")
        return value
    if type(value) in (list, tuple) and len(value) <= 1000:
        for item in value:
            bounded(item, depth + 1, budget)
        return list(value)
    if type(value) is dict and len(value) <= 1000:
        for key, item in value.items():
            if type(key) is not str:
                raise ValueError("object keys must be strings")
            bounded(key, depth + 1, budget)
            bounded(item, depth + 1, budget)
        return value
    raise ValueError("value exceeds pure function bounds")


def parse(code):
    if type(code) is not str or not 1 <= len(code.encode()) <= 16384:
        raise ValueError("function source exceeds bounds")
    tree = ast.parse(code)
    if len(tree.body) != 1 or not isinstance(tree.body[0], ast.FunctionDef):
        raise ValueError("define exactly one function")
    fn = tree.body[0]
    a = fn.args
    if (fn.decorator_list or fn.returns or fn.type_comment or len(a.args) != 1
            or a.args[0].arg != "args" or a.args[0].annotation or a.posonlyargs
            or a.kwonlyargs or a.vararg or a.kwarg or a.defaults or a.kw_defaults
            or len(fn.body) != 1 or not isinstance(fn.body[0], ast.Return)
            or fn.body[0].value is None):
        raise ValueError("use def name(args): return one_expression")
    expression = fn.body[0].value
    nodes = list(ast.walk(expression))
    if len(nodes) > 512:
        raise ValueError("expression exceeds bounds")
    allowed = (ast.Constant, ast.Name, ast.Load, ast.List, ast.Tuple, ast.Dict,
               ast.Subscript, ast.BinOp, ast.UnaryOp, ast.BoolOp, ast.Compare,
               ast.IfExp, ast.Call, ast.And, ast.Or, ast.Not, ast.UAdd, ast.USub,
               *OPS, *COMPARE)
    callees={id(node.func) for node in nodes if isinstance(node,ast.Call)}
    for node in nodes:
        if not isinstance(node, allowed):
            raise ValueError("unsupported pure Python expression")
        if isinstance(node, ast.Name) and node.id != "args" and (node.id not in CALLS or id(node) not in callees):
            raise ValueError("unknown function or variable")
        if isinstance(node, ast.Call) and (not isinstance(node.func, ast.Name)
                or node.func.id not in CALLS or node.keywords or len(node.args) > 4):
            raise ValueError("only bounded pure builtin calls are allowed")
        if isinstance(node, ast.Dict) and any(key is None for key in node.keys):
            raise ValueError("dictionary unpacking is unsupported")
        if isinstance(node, ast.Constant):
            bounded(node.value)
    return fn.name, expression


def invoke(code, arguments):
    _, expression = parse(code)
    bounded(arguments)
    if len(json.dumps(arguments).encode()) > 32768:
        raise ValueError("arguments exceed bounds")
    steps = 0
    def evaluate(node):
        nonlocal steps
        steps += 1
        if steps > 2048:
            raise ValueError("expression step budget exceeded")
        if isinstance(node, ast.Constant):
            value = node.value
        elif isinstance(node, ast.Name) and node.id == "args":
            value = arguments
        elif isinstance(node, (ast.List, ast.Tuple)):
            value = [evaluate(item) for item in node.elts]
        elif isinstance(node, ast.Dict):
            value = {evaluate(key): evaluate(item) for key,item in zip(node.keys,node.values)}
        elif isinstance(node, ast.Subscript):
            value = evaluate(node.value)[evaluate(node.slice)]
        elif isinstance(node, ast.BinOp):
            a,b = evaluate(node.left),evaluate(node.right)
            op = type(node.op)
            if op is ast.Mult and (type(a) in (str,list) or type(b) in (str,list)):
                seq,n = (a,b) if type(a) in (str,list) else (b,a)
                if type(n) is not int or n < 0 or n * len(seq) > 1000:
                    raise ValueError("sequence multiplication exceeds bounds")
            if op is ast.Mod and type(a) is str:
                raise ValueError("string formatting is unsupported")
            value = OPS[op](a,b)
        elif isinstance(node, ast.UnaryOp):
            a = evaluate(node.operand)
            value = not a if isinstance(node.op,ast.Not) else +a if isinstance(node.op,ast.UAdd) else -a
        elif isinstance(node, ast.BoolOp):
            value = evaluate(node.values[0])
            for item in node.values[1:]:
                if (isinstance(node.op,ast.And) and not value) or (isinstance(node.op,ast.Or) and value):
                    break
                value = evaluate(item)
        elif isinstance(node, ast.Compare):
            a = evaluate(node.left)
            value = True
            for op,item in zip(node.ops,node.comparators):
                b = evaluate(item)
                if not COMPARE[type(op)](a,b):
                    value = False
                    break
                a = b
        elif isinstance(node, ast.IfExp):
            value = evaluate(node.body if evaluate(node.test) else node.orelse)
        elif isinstance(node, ast.Call):
            value = CALLS[node.func.id](*[evaluate(item) for item in node.args])
        else:
            raise ValueError("unsupported expression value")
        return bounded(value)
    output = evaluate(expression)
    if len(json.dumps(output,allow_nan=False).encode()) > 32768:
        raise ValueError("output exceeds bounds")
    return output
