//! calculate: bounded finite f64 arithmetic.
use anyhow::{bail, Result};
use serde::Deserialize;
use serde_json::{json, Value};

const MAX_NUMBER: f64 = 1e100;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Arithmetic {
    operation: String,
    operands: Vec<f64>,
}
fn finite(value: f64) -> Result<f64> {
    if !value.is_finite() || value.abs() > MAX_NUMBER {
        bail!("Arithmetic result exceeds finite magnitude limit 1e100");
    }
    Ok(if value == 0.0 { 0.0 } else { value })
}
pub(super) fn arithmetic(args: Arithmetic) -> Result<Value> {
    // The operand count limit lives in calculate.tool.json, which the engine enforces.
    let Some(&first) = args.operands.first() else {
        bail!("Expected at least one operand");
    };
    for &operand in &args.operands {
        finite(operand)?;
    }
    let unary = matches!(args.operation.as_str(), "sqrt" | "abs");
    let binary = matches!(args.operation.as_str(), "power" | "remainder");
    if unary && args.operands.len() != 1 || binary && args.operands.len() != 2 {
        bail!("Wrong operand count for operation");
    }
    let result = match args.operation.as_str() {
        "sqrt" => {
            if first < 0.0 {
                bail!("Square root requires a nonnegative operand");
            }
            finite(first.sqrt())?
        }
        "abs" => finite(first.abs())?,
        "power" => {
            if args.operands[1].abs() > 1024.0 {
                bail!("Exponent magnitude exceeds 1024");
            }
            finite(first.powf(args.operands[1]))?
        }
        "remainder" => {
            if args.operands[1] == 0.0 {
                bail!("Division by zero");
            }
            finite(first % args.operands[1])?
        }
        "add" | "subtract" | "multiply" | "divide" | "min" | "max" => {
            let mut result = first;
            for &next in &args.operands[1..] {
                result = finite(match args.operation.as_str() {
                    "add" => result + next,
                    "subtract" => result - next,
                    "multiply" => result * next,
                    "divide" => {
                        if next == 0.0 {
                            bail!("Division by zero");
                        }
                        result / next
                    }
                    "min" => result.min(next),
                    "max" => result.max(next),
                    _ => unreachable!(),
                })?;
            }
            result
        }
        _ => bail!("Unsupported arithmetic operation"),
    };
    Ok(
        json!({"result":result,"operation":args.operation,"operand_count":args.operands.len(),"precision":"f64"}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    fn calc(op: &str, numbers: Value) -> Result<Value> {
        arithmetic(serde_json::from_value(
            json!({"operation":op,"operands":numbers}),
        )?)
    }
    #[test]
    fn arithmetic_is_finite_and_never_interprets_code() {
        assert_eq!(calc("subtract", json!([12, 2, 3])).unwrap()["result"], 7.0);
        assert_eq!(calc("power", json!([2, 10])).unwrap()["result"], 1024.0);
        assert_eq!(calc("sqrt", json!([144])).unwrap()["result"], 12.0);
        for (op, numbers) in [
            ("divide", json!([1, 0])),
            ("multiply", json!([1e100, 10])),
            ("power", json!([10, 1025])),
            ("power", json!([-2, 0.5])),
            ("sqrt", json!([-1])),
            ("abs", json!([1, 2])),
            ("remainder", json!([1])),
            ("add", json!([])),
            ("eval", json!([1])),
        ] {
            assert!(calc(op, numbers).is_err());
        }
        assert!(serde_json::from_value::<Arithmetic>(
            json!({"operation":"add","operands":[1],"expression":"1+1"})
        )
        .is_err());
        assert!(calc("add", json!([1e101])).is_err());
    }
}
