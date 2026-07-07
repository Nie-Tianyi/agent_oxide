//! [`CalculatorTool`] — 四则运算表达式求值工具。
//!
//! # 表达式求值器
//!
//! 手写递归下降解析器，支持以下运算（按优先级从低到高）：
//!
//! | 优先级 | 运算符 | 结合性 |
//! |--------|--------|--------|
//! | 1 (低) | `+` `-` | 左结合 |
//! | 2 (高) | `*` `/` | 左结合 |
//! | 3 (前缀) | `+` `-` | 右结合（一元） |
//! | - | `( )` | 分组 |
//!
//! 文法：
//! ```text
//! expr   → term (('+' | '-') term)*
//! term   → unary (('*' | '/') unary)*
//! unary  → ('+' | '-') unary | primary
//! primary → NUMBER | '(' expr ')'
//! ```
//!
//! # 实现选择
//!
//! 手写解析器而非引入 `meval` 等 crate，原因：
//! - 零额外依赖
//! - 展示递归下降解析器的经典实现模式（教学目的）
//! - 对于教学项目来说，~100 行的解析器比一个黑盒 crate 更有价值

use serde_json::Value;

use super::ToolError;
use super::tool::{Tool, extract_string_arg};

// ── CalculatorTool ────────────────────────────────────────────────────────────

/// 安全地求值数学表达式。
///
/// # 示例输入
///
/// ```json
/// {"expression": "2 + 3 * (4 - 1)"}
/// ```
///
/// # 错误
///
/// - 除零 → [`ToolError::Execution`]
/// - 非法字符（如 `^`）→ [`ToolError::Execution`]
/// - 缺少 `expression` 字段 → [`ToolError::InvalidArgs`]
pub struct CalculatorTool;

impl Tool for CalculatorTool {
    fn name(&self) -> &str {
        "calculator"
    }

    fn description(&self) -> &str {
        "Evaluate a mathematical expression and return the numeric result. Supports \
         standard arithmetic with correct operator precedence.\n\n\
         Supported operators: + (addition), - (subtraction), * (multiplication), \
         / (division), () (grouping), unary + and - (e.g. -5 + 3).\n\
         Supported numbers: integers and decimals (e.g. 42, 3.14, -0.5).\n\
         NOT supported: ^ (exponentiation, use repeated multiplication), % (modulo), \
         sqrt/sin/cos/log (no functions), hex/binary/octal literals, variables.\n\n\
         Example expressions:\n\
         - `2 + 3 * 4` → 14 (multiplication before addition)\n\
         - `(100 - 20) / 4` → 20 (parentheses first)\n\
         - `-5 + 3.14 * 2` → 1.28 (unary minus + decimal)\n\n\
         When NOT to use: complex math beyond basic arithmetic, unit conversions \
         requiring lookup tables, statistical analysis (use shell + a script)."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "expression": {
                    "type": "string",
                    "description": "A mathematical expression using +, -, *, /, parentheses, and decimal numbers. Examples: '2 + 3 * 4', '(100 - 20) / 4', '-5 + 3.14 * 2'. No functions, no variables, no exponentiation."
                }
            },
            "required": ["expression"],
            "additionalProperties": false
        })
    }

    fn execute(&self, args: &str) -> Result<String, ToolError> {
        let expression = extract_string_arg(args, "expression")?;

        let result = ExprEvaluator::evaluate(&expression).map_err(|e| {
            ToolError::Execution(format!("at position {}: {e}", e.position.unwrap_or(0)))
        })?;

        // 整数结果去掉尾随 ".0"
        if result == result.trunc() && result.is_finite() {
            Ok(format!("{}", result as i64))
        } else {
            Ok(format!("{result}"))
        }
    }
}

// ── Expression evaluator ──────────────────────────────────────────────────────

/// 解析错误信息。
#[derive(Debug, Clone, PartialEq)]
struct ParseError {
    message: String,
    position: Option<usize>,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

// ── Token ─────────────────────────────────────────────────────────────────────

/// 词法单元。
#[derive(Debug, Clone, PartialEq)]
enum Token {
    Number(f64),
    Plus,
    Minus,
    Star,
    Slash,
    LParen,
    RParen,
}

// ── Lexer ─────────────────────────────────────────────────────────────────────

/// 词法分析器：将字符串切分为 [`Token`] 流。
///
/// 跳过空白字符，将运算符映射为对应 Token，数字字面量解析为 `f64`。
struct Lexer {
    chars: Vec<char>,
    pos: usize,
}

impl Lexer {
    fn new(input: &str) -> Self {
        Self {
            chars: input.chars().collect(),
            pos: 0,
        }
    }

    /// 当前字符位置（用于错误报告）。
    fn current_pos(&self) -> usize {
        self.pos
    }

    /// 消费并返回下一个 token。到达输入末尾时返回 `None`。
    fn next_token(&mut self) -> Option<Token> {
        self.skip_whitespace();

        let ch = self.peek()?;

        Some(match ch {
            '+' => {
                self.pos += 1;
                Token::Plus
            }
            '-' => {
                self.pos += 1;
                Token::Minus
            }
            '*' => {
                self.pos += 1;
                Token::Star
            }
            '/' => {
                self.pos += 1;
                Token::Slash
            }
            '(' => {
                self.pos += 1;
                Token::LParen
            }
            ')' => {
                self.pos += 1;
                Token::RParen
            }
            c if c.is_ascii_digit() || c == '.' => self.read_number(),
            _ => {
                // 非法字符 — 不推进位置，让 evaluate() 的尾部检查捕获
                return None;
            }
        })
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn skip_whitespace(&mut self) {
        while self.pos < self.chars.len() && self.chars[self.pos].is_whitespace() {
            self.pos += 1;
        }
    }

    fn read_number(&mut self) -> Token {
        let start = self.pos;
        let mut dot_seen = self.chars[self.pos] == '.';
        self.pos += 1;

        while self.pos < self.chars.len() {
            let c = self.chars[self.pos];
            if c.is_ascii_digit() {
                self.pos += 1;
            } else if c == '.' && !dot_seen {
                dot_seen = true;
                self.pos += 1;
            } else {
                break;
            }
        }

        let num_str: String = self.chars[start..self.pos].iter().collect();

        // Reject malformed numbers: "." alone, or numbers with multiple dots like "1..5"
        if num_str == "." {
            return Token::Number(f64::NAN);
        }
        if num_str.matches('.').count() > 1 {
            return Token::Number(f64::NAN);
        }

        let num = num_str.parse::<f64>().unwrap_or(f64::NAN);
        Token::Number(num)
    }
}

// ── Parser ────────────────────────────────────────────────────────────────────

/// 递归下降解析器。
///
/// 持有 [`Lexer`] 和一个前瞻 token（lookahead），
/// 通过 `advance()` 消费 token 并加载下一个。
struct Parser {
    lexer: Lexer,
    lookahead: Option<Token>,
    /// 当前 token 在输入中的位置，用于错误报告。
    token_pos: usize,
}

impl Parser {
    fn new(input: &str) -> Self {
        let mut lexer = Lexer::new(input);
        let token_pos = lexer.current_pos();
        let lookahead = lexer.next_token();
        Self {
            lexer,
            lookahead,
            token_pos,
        }
    }

    fn evaluate(input: &str) -> Result<f64, ParseError> {
        let mut parser = Self::new(input);
        let result = parser.parse_expr()?;

        // 检查一：不应有未被消费的合法 token
        if parser.lookahead.is_some() {
            return Err(ParseError {
                message: "unexpected token after expression".into(),
                position: Some(parser.token_pos),
            });
        }

        // 检查二：所有输入字符都应被消费（跳过尾部空白后）
        parser.lexer.skip_whitespace();
        if parser.lexer.current_pos() < parser.lexer.chars.len() {
            return Err(ParseError {
                message: "unexpected character after expression".into(),
                position: Some(parser.lexer.current_pos()),
            });
        }

        Ok(result)
    }

    /// 检查当前 token 是否匹配，若匹配则消费。
    fn eat(&mut self, expected: Token) -> bool {
        if self.lookahead == Some(expected) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn advance(&mut self) {
        self.token_pos = self.lexer.current_pos();
        self.lookahead = self.lexer.next_token();
    }

    fn error(&self, msg: &str) -> ParseError {
        ParseError {
            message: msg.into(),
            position: Some(self.token_pos),
        }
    }

    // expr → term (('+' | '-') term)*
    fn parse_expr(&mut self) -> Result<f64, ParseError> {
        let mut left = self.parse_term()?;

        loop {
            if self.eat(Token::Plus) {
                left += self.parse_term()?;
            } else if self.eat(Token::Minus) {
                left -= self.parse_term()?;
            } else {
                break;
            }
        }

        Ok(left)
    }

    // term → unary (('*' | '/') unary)*
    fn parse_term(&mut self) -> Result<f64, ParseError> {
        let mut left = self.parse_unary()?;

        loop {
            if self.eat(Token::Star) {
                left *= self.parse_unary()?;
            } else if self.eat(Token::Slash) {
                let right = self.parse_unary()?;
                if right == 0.0 {
                    return Err(ParseError {
                        message: "division by zero".into(),
                        position: None,
                    });
                }
                left /= right;
            } else {
                break;
            }
        }

        Ok(left)
    }

    // unary → ('+' | '-') unary | primary
    fn parse_unary(&mut self) -> Result<f64, ParseError> {
        if self.eat(Token::Plus) {
            // 一元加号：直接穿透
            self.parse_unary()
        } else if self.eat(Token::Minus) {
            // 一元减号：取反
            Ok(-self.parse_unary()?)
        } else {
            self.parse_primary()
        }
    }

    // primary → NUMBER | '(' expr ')'
    fn parse_primary(&mut self) -> Result<f64, ParseError> {
        match self.lookahead.take() {
            Some(Token::Number(n)) if n.is_finite() => {
                self.advance();
                Ok(n)
            }
            Some(Token::Number(_)) => {
                self.advance();
                Err(self.error("invalid number"))
            }
            Some(Token::LParen) => {
                self.advance();
                let val = self.parse_expr()?;
                if !self.eat(Token::RParen) {
                    return Err(self.error("expected ')'"));
                }
                Ok(val)
            }
            Some(_) => Err(self.error("unexpected token")),
            None => Err(ParseError {
                message: "unexpected end of expression".into(),
                position: None,
            }),
        }
    }
}

/// 便捷入口：对输入字符串求值。
type ExprEvaluator = Parser;

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── CalculatorTool 集成测试 ─────────────────────────────

    #[test]
    fn test_name() {
        assert_eq!(CalculatorTool.name(), "calculator");
    }

    #[test]
    fn test_description() {
        assert!(
            CalculatorTool
                .description()
                .contains("mathematical expression")
        );
    }

    #[test]
    fn test_parameters_schema() {
        let params = CalculatorTool.parameters();
        assert_eq!(params["type"], "object");
        assert!(params["properties"]["expression"]["type"] == "string");
        assert!(
            params["required"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("expression"))
        );
    }

    #[test]
    fn test_addition() {
        assert_eq!(calc("2 + 3"), "5");
    }

    #[test]
    fn test_subtraction() {
        assert_eq!(calc("10 - 7"), "3");
    }

    #[test]
    fn test_multiplication() {
        assert_eq!(calc("4 * 5"), "20");
    }

    #[test]
    fn test_division() {
        assert_eq!(calc("15 / 3"), "5");
    }

    #[test]
    fn test_operator_precedence() {
        assert_eq!(calc("2 + 3 * 4"), "14");
    }

    #[test]
    fn test_parentheses() {
        assert_eq!(calc("(2 + 3) * 4"), "20");
    }

    #[test]
    fn test_unary_minus() {
        assert_eq!(calc("-5 + 3"), "-2");
    }

    #[test]
    fn test_unary_plus() {
        assert_eq!(calc("+5 - 2"), "3");
    }

    #[test]
    fn test_decimal() {
        assert_eq!(calc("3.5 + 1.5"), "5");
    }

    #[test]
    fn test_nested_parentheses() {
        assert_eq!(calc("((2 + 3) * (4 - 1)) / 5"), "3");
    }

    #[test]
    fn test_float_result() {
        assert!(calc("10 / 3").starts_with("3.33"));
    }

    #[test]
    fn test_division_by_zero() {
        let err = CalculatorTool
            .execute(r#"{"expression": "1 / 0"}"#)
            .unwrap_err();
        assert!(matches!(err, ToolError::Execution(_)));
        assert!(err.to_string().contains("division by zero"));
    }

    #[test]
    fn test_invalid_json() {
        let err = CalculatorTool.execute("garbage").unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[test]
    fn test_missing_expression_field() {
        let err = CalculatorTool.execute(r#"{"wrong": "field"}"#).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[test]
    fn test_empty_expression() {
        let err = calc_err("");
        assert!(matches!(err, ToolError::Execution(_)));
    }

    #[test]
    fn test_unsupported_operator() {
        let err = calc_err("2 ^ 3");
        assert!(matches!(err, ToolError::Execution(_)));
    }

    #[test]
    fn test_trailing_operator() {
        let err = calc_err("2 *");
        assert!(matches!(err, ToolError::Execution(_)));
    }

    #[test]
    fn test_mismatched_parens() {
        let err = calc_err("(2 + 3");
        assert!(matches!(err, ToolError::Execution(_)));
    }

    // ── 解析器单元测试 ─────────────────────────────────────

    #[test]
    fn test_parser_simple_addition() {
        assert_eq!(Parser::evaluate("1 + 2").unwrap(), 3.0);
    }

    #[test]
    fn test_parser_whitespace_insensitive() {
        assert_eq!(Parser::evaluate("  1   +   2  ").unwrap(), 3.0);
    }

    #[test]
    fn test_parser_trailing_garbage() {
        assert!(Parser::evaluate("1 + 2 x").is_err());
    }

    #[test]
    fn test_lexer_empty_input() {
        let mut lexer = Lexer::new("");
        assert!(lexer.next_token().is_none());
    }

    #[test]
    fn test_lexer_only_whitespace() {
        let mut lexer = Lexer::new("   \t  ");
        assert!(lexer.next_token().is_none());
    }

    // ── 辅助函数 ───────────────────────────────────────────

    /// 用 CalculatorTool 执行表达式，返回成功结果。
    fn calc(expr: &str) -> String {
        CalculatorTool
            .execute(&format!(r#"{{"expression": "{}"}}"#, expr))
            .unwrap()
    }

    /// 用 CalculatorTool 执行表达式，返回错误。
    fn calc_err(expr: &str) -> ToolError {
        CalculatorTool
            .execute(&format!(r#"{{"expression": "{}"}}"#, expr))
            .unwrap_err()
    }
}
