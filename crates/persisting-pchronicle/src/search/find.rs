//! Parser and syntax tree for the unified `pchronicle find --match` expression.

use anyhow::{Result, bail};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FindTextField {
    Content,
    Message,
    User,
    Assistant,
    System,
    Reasoning,
    Observation,
    Prompt,
    Model,
    Env,
    All,
}

impl FindTextField {
    pub fn parse(name: &str) -> Result<Self> {
        match name {
            "content" => Ok(Self::Content),
            "message" => Ok(Self::Message),
            "user" => Ok(Self::User),
            "assistant" | "agent" => Ok(Self::Assistant),
            "system" => Ok(Self::System),
            "reasoning" => Ok(Self::Reasoning),
            "observation" => Ok(Self::Observation),
            "prompt" => Ok(Self::Prompt),
            "model" | "model_name" => Ok(Self::Model),
            "env" => Ok(Self::Env),
            "all" => Ok(Self::All),
            _ => bail!(
                "unknown find text field '#{name}'; expected #content, #message, #user, #assistant, #system, #reasoning, #observation, #prompt, #model, #env, or #all"
            ),
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Content => "content",
            Self::Message => "message",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::System => "system",
            Self::Reasoning => "reasoning",
            Self::Observation => "observation",
            Self::Prompt => "prompt",
            Self::Model => "model",
            Self::Env => "env",
            Self::All => "all",
        }
    }

    pub fn source_predicate(self) -> Option<&'static str> {
        match self {
            Self::User => Some("source = 'user'"),
            Self::Assistant => Some("source = 'agent'"),
            Self::System => Some("source = 'system'"),
            _ => None,
        }
    }

    pub fn columns(&self) -> &'static [&'static str] {
        match self {
            Self::Reasoning => &["reasoning_content"],
            Self::Message | Self::User | Self::Assistant => &["message_value"],
            Self::Observation => &["observation"],
            Self::Prompt | Self::System => &["prompt", "message_value"],
            Self::Model => &["model_name"],
            Self::Env => &["env"],
            Self::Content => &["message_value", "observation", "prompt"],
            Self::All => &[
                "message_value",
                "reasoning_content",
                "model_name",
                "observation",
                "env",
                "prompt",
            ],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FindTextPredicate {
    pub field: FindTextField,
    pub query: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindJsonOperator {
    Eq,
    NotEq,
    Gt,
    Gte,
    Lt,
    Lte,
}

impl FindJsonOperator {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Eq => "=",
            Self::NotEq => "!=",
            Self::Gt => ">",
            Self::Gte => ">=",
            Self::Lt => "<",
            Self::Lte => "<=",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FindJsonPredicate {
    pub column: Option<String>,
    pub path: String,
    pub operator: FindJsonOperator,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FindExpr {
    Text(FindTextPredicate),
    Json(FindJsonPredicate),
    And(Vec<FindExpr>),
    Or(Vec<FindExpr>),
    Not(Box<FindExpr>),
}

impl FindExpr {
    pub fn and(expressions: Vec<Self>) -> Option<Self> {
        let mut expressions = expressions;
        match expressions.len() {
            0 => None,
            1 => Some(expressions.pop().expect("one expression")),
            _ => Some(Self::And(expressions)),
        }
    }

    pub fn has_text(&self) -> bool {
        match self {
            Self::Text(_) => true,
            Self::Json(_) => false,
            Self::And(items) | Self::Or(items) => items.iter().any(Self::has_text),
            Self::Not(item) => item.has_text(),
        }
    }

    pub fn has_json(&self) -> bool {
        match self {
            Self::Text(_) => false,
            Self::Json(_) => true,
            Self::And(items) | Self::Or(items) => items.iter().any(Self::has_json),
            Self::Not(item) => item.has_json(),
        }
    }

    /// Explicit step JSON columns (currently `metrics`) force a Step lookup
    /// even when the expression contains no text predicate.
    pub fn has_step_json(&self) -> bool {
        match self {
            Self::Text(_) => false,
            Self::Json(predicate) => predicate.column.as_deref() == Some("metrics"),
            Self::And(items) | Self::Or(items) => items.iter().any(Self::has_step_json),
            Self::Not(item) => item.has_step_json(),
        }
    }

    pub fn display(&self) -> String {
        match self {
            Self::Text(predicate) => format!(
                "#{}({})",
                predicate.field.display_name(),
                quote_for_display(&predicate.query)
            ),
            Self::Json(predicate) => {
                let field = predicate
                    .column
                    .as_deref()
                    .map(|column| format!("#json.{column}"))
                    .unwrap_or_else(|| "#json".into());
                format!(
                    "{}({}) {} {}",
                    field,
                    quote_for_display(&predicate.path),
                    predicate.operator.as_str(),
                    predicate.value
                )
            }
            Self::And(items) => items
                .iter()
                .map(Self::display)
                .collect::<Vec<_>>()
                .join(" AND "),
            Self::Or(items) => items
                .iter()
                .map(Self::display)
                .collect::<Vec<_>>()
                .join(" OR "),
            Self::Not(item) => format!("NOT ({})", item.display()),
        }
    }
}

pub fn combine_match_expressions(expressions: &[String]) -> Result<Option<FindExpr>> {
    expressions
        .iter()
        .map(|expression| parse_match_expression(expression))
        .collect::<Result<Vec<_>>>()
        .map(FindExpr::and)
}

pub fn parse_match_expression(input: &str) -> Result<FindExpr> {
    let trimmed = input.trim();
    if is_plain_text_query(trimmed) {
        return Ok(FindExpr::Text(FindTextPredicate {
            field: FindTextField::Content,
            query: unquote(trimmed)?,
        }));
    }
    let mut parser = Parser { input, offset: 0 };
    parser.skip_ws();
    if parser.is_eof() {
        bail!("--match must not be empty");
    }
    let expression = parser.parse_or()?;
    parser.skip_ws();
    if !parser.is_eof() {
        bail!(
            "unexpected token in --match near '{}'",
            parser.remaining_preview()
        );
    }
    Ok(expression)
}

fn is_plain_text_query(input: &str) -> bool {
    if input.len() >= 2 {
        let bytes = input.as_bytes();
        if matches!(
            (bytes[0], bytes[input.len() - 1]),
            (b'"', b'"') | (b'\'', b'\'')
        ) {
            return true;
        }
    }
    !input.is_empty()
        && !input
            .chars()
            .any(|character| matches!(character, '#' | '$' | '(' | ')'))
        && !input
            .split_whitespace()
            .any(|token| matches!(token.to_ascii_uppercase().as_str(), "AND" | "OR" | "NOT"))
}

struct Parser<'a> {
    input: &'a str,
    offset: usize,
}

impl<'a> Parser<'a> {
    fn parse_or(&mut self) -> Result<FindExpr> {
        let mut items = vec![self.parse_and()?];
        loop {
            let checkpoint = self.offset;
            if !self.consume_keyword("OR") {
                self.offset = checkpoint;
                break;
            }
            items.push(self.parse_and()?);
        }
        Ok(if items.len() == 1 {
            items.pop().expect("one expression")
        } else {
            FindExpr::Or(items)
        })
    }

    fn parse_and(&mut self) -> Result<FindExpr> {
        let mut items = vec![self.parse_unary()?];
        loop {
            let checkpoint = self.offset;
            if !self.consume_keyword("AND") {
                self.offset = checkpoint;
                break;
            }
            items.push(self.parse_unary()?);
        }
        Ok(if items.len() == 1 {
            items.pop().expect("one expression")
        } else {
            FindExpr::And(items)
        })
    }

    fn parse_unary(&mut self) -> Result<FindExpr> {
        self.skip_ws();
        if self.consume_keyword("NOT") {
            return Ok(FindExpr::Not(Box::new(self.parse_unary()?)));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<FindExpr> {
        self.skip_ws();
        if self.consume_char('(') {
            let expression = self.parse_or()?;
            self.skip_ws();
            if !self.consume_char(')') {
                bail!("missing ')' in --match expression")
            }
            return Ok(expression);
        }
        if self.peek_char() == Some('#') {
            return self.parse_hash_predicate();
        }
        let token = self.read_atom_token()?;
        if token.starts_with('$') {
            return Ok(FindExpr::Json(parse_json_spec(&token, None)?));
        }
        Ok(FindExpr::Text(FindTextPredicate {
            field: FindTextField::Content,
            query: unquote(&token)?,
        }))
    }

    fn parse_hash_predicate(&mut self) -> Result<FindExpr> {
        self.consume_char('#');
        let name = self.read_identifier("field name after '#'")?;
        self.skip_ws();
        if !self.consume_char('(') {
            bail!("find field '#{name}' must use '#{name}(...)'")
        }
        let argument = self.read_balanced_argument()?;
        if name == "json" || name.starts_with("json.") {
            self.skip_ws();
            let operator = self.read_json_operator()?;
            let value = self.read_atom_token()?;
            let column = name.strip_prefix("json.").map(str::to_owned);
            let path = unquote(argument.trim())?;
            return Ok(FindExpr::Json(FindJsonPredicate {
                column,
                path,
                operator,
                value: parse_json_value(&value)?,
            }));
        }
        let field = FindTextField::parse(&name)?;
        let query = unquote(argument.trim())?;
        if query.trim().is_empty() {
            bail!("find text field '#{name}' must not be empty");
        }
        Ok(FindExpr::Text(FindTextPredicate { field, query }))
    }

    fn read_balanced_argument(&mut self) -> Result<String> {
        let start = self.offset;
        let mut depth = 1usize;
        let mut quote = None;
        while let Some((index, character)) = self.input[self.offset..].char_indices().next() {
            let absolute = self.offset + index;
            self.offset = absolute + character.len_utf8();
            if let Some(expected) = quote {
                if character == expected {
                    quote = None;
                }
                continue;
            }
            if character == '\'' || character == '"' {
                quote = Some(character);
            } else if character == '(' {
                depth += 1;
            } else if character == ')' {
                depth -= 1;
                if depth == 0 {
                    return Ok(self.input[start..absolute].to_owned());
                }
            }
        }
        bail!("missing ')' in --match field expression")
    }

    fn read_json_operator(&mut self) -> Result<FindJsonOperator> {
        for (token, operator) in [
            (">=", FindJsonOperator::Gte),
            ("<=", FindJsonOperator::Lte),
            ("!=", FindJsonOperator::NotEq),
            ("=", FindJsonOperator::Eq),
            (">", FindJsonOperator::Gt),
            ("<", FindJsonOperator::Lt),
        ] {
            if self.input[self.offset..].starts_with(token) {
                self.offset += token.len();
                return Ok(operator);
            }
        }
        bail!("JSON find predicate must use one of =, !=, >, >=, <, <=")
    }

    fn read_identifier(&mut self, label: &str) -> Result<String> {
        let start = self.offset;
        while let Some(character) = self.peek_char() {
            if character.is_ascii_alphanumeric() || character == '_' || character == '.' {
                self.offset += character.len_utf8();
            } else {
                break;
            }
        }
        if self.offset == start {
            bail!("missing {label}");
        }
        Ok(self.input[start..self.offset].to_owned())
    }

    fn read_atom_token(&mut self) -> Result<String> {
        self.skip_ws();
        let start = self.offset;
        let mut quote = None;
        while let Some(character) = self.peek_char() {
            if let Some(expected) = quote {
                self.offset += character.len_utf8();
                if character == expected {
                    quote = None;
                }
                continue;
            }
            if character == '\'' || character == '"' {
                quote = Some(character);
                self.offset += character.len_utf8();
            } else if character.is_whitespace() || character == '(' || character == ')' {
                break;
            } else {
                self.offset += character.len_utf8();
            }
        }
        if quote.is_some() {
            bail!("unterminated quote in --match expression");
        }
        if self.offset == start {
            bail!(
                "expected a find predicate near '{}'",
                self.remaining_preview()
            );
        }
        Ok(self.input[start..self.offset].to_owned())
    }

    fn consume_keyword(&mut self, keyword: &str) -> bool {
        self.skip_ws();
        let end = self.offset + keyword.len();
        if !self.input[self.offset..]
            .get(..keyword.len())
            .is_some_and(|value| value.eq_ignore_ascii_case(keyword))
        {
            return false;
        }
        let boundary = self.input[end..].chars().next();
        if boundary.is_some_and(|value| value.is_ascii_alphanumeric() || value == '_') {
            return false;
        }
        self.offset = end;
        true
    }

    fn skip_ws(&mut self) {
        while let Some(character) = self.peek_char() {
            if !character.is_whitespace() {
                break;
            }
            self.offset += character.len_utf8();
        }
    }

    fn consume_char(&mut self, expected: char) -> bool {
        if self.peek_char() == Some(expected) {
            self.offset += expected.len_utf8();
            true
        } else {
            false
        }
    }

    fn peek_char(&self) -> Option<char> {
        self.input[self.offset..].chars().next()
    }

    fn is_eof(&self) -> bool {
        self.offset >= self.input.len()
    }

    fn remaining_preview(&self) -> String {
        self.input[self.offset..].chars().take(24).collect()
    }
}

fn parse_json_spec(spec: &str, column: Option<String>) -> Result<FindJsonPredicate> {
    let (path, operator, raw_value) = split_json_spec(spec)?;
    Ok(FindJsonPredicate {
        column,
        path: path.to_owned(),
        operator,
        value: parse_json_value(raw_value)?,
    })
}

fn split_json_spec(spec: &str) -> Result<(&str, FindJsonOperator, &str)> {
    anyhow::ensure!(spec.starts_with('$'), "JSON find path must start with '$'");
    for (token, operator) in [
        (">=", FindJsonOperator::Gte),
        ("<=", FindJsonOperator::Lte),
        ("!=", FindJsonOperator::NotEq),
        ("=", FindJsonOperator::Eq),
        (">", FindJsonOperator::Gt),
        ("<", FindJsonOperator::Lt),
    ] {
        if let Some(index) = spec.find(token) {
            let path = &spec[..index];
            let value = &spec[index + token.len()..];
            anyhow::ensure!(!path.is_empty(), "JSON find path must not be empty");
            anyhow::ensure!(!value.is_empty(), "JSON find value must not be empty");
            return Ok((path, operator, value));
        }
    }
    bail!("JSON find predicate must use PATH=VALUE")
}

fn parse_json_value(raw: &str) -> Result<Value> {
    let raw = raw.trim();
    if raw.is_empty() {
        bail!("JSON find value must not be empty");
    }
    if let Ok(value) = serde_json::from_str(raw) {
        return Ok(value);
    }
    Ok(Value::String(unquote(raw)?))
}

fn unquote(value: &str) -> Result<String> {
    let value = value.trim();
    if value.len() >= 2 {
        let first = value.as_bytes()[0] as char;
        let last = value.as_bytes()[value.len() - 1] as char;
        if (first == '"' && last == '"') || (first == '\'' && last == '\'') {
            if first == '"' {
                return Ok(serde_json::from_str(value)?);
            }
            return Ok(value[1..value.len() - 1].replace("\\'", "'"));
        }
    }
    Ok(value.to_owned())
}

fn quote_for_display(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| format!("'{value}'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_terms_and_repeated_terms() {
        let expression = combine_match_expressions(&["ipython".into(), "task".into()]).unwrap();
        assert!(matches!(expression, Some(FindExpr::And(items)) if items.len() == 2));
    }

    #[test]
    fn parses_unquoted_plain_phrases_as_one_content_query() {
        let expression = parse_match_expression("ipython task").unwrap();
        assert!(matches!(expression, FindExpr::Text(predicate)
            if predicate.field == FindTextField::Content && predicate.query == "ipython task"));
    }

    #[test]
    fn parses_scoped_boolean_text() {
        let expression = parse_match_expression(
            r#"(#user("timeout") OR #assistant("retry")) AND NOT #system(ipython)"#,
        )
        .unwrap();
        assert!(expression.has_text());
        assert!(!expression.has_json());
        assert!(expression.display().contains("#user"));
    }

    #[test]
    fn parses_indexed_field_selectors() {
        let expression = parse_match_expression("#observation(timeout)").unwrap();
        assert!(matches!(expression, FindExpr::Text(predicate)
            if predicate.field == FindTextField::Observation
                && predicate.field.columns() == ["observation"]));
        let expression = parse_match_expression("#model(gpt)").unwrap();
        assert!(matches!(expression, FindExpr::Text(predicate)
            if predicate.field == FindTextField::Model));
    }

    #[test]
    fn parses_json_shorthand_and_typed_values() {
        let expression = parse_match_expression(r#"$.tags=important AND $.priority=2"#).unwrap();
        let FindExpr::And(items) = expression else {
            panic!("expected AND expression");
        };
        assert!(
            matches!(&items[0], FindExpr::Json(predicate) if predicate.value == Value::String("important".into()))
        );
        assert!(
            matches!(&items[1], FindExpr::Json(predicate) if predicate.value == Value::Number(2.into()))
        );
        assert!(items[0].display().contains("= \"important\""));
    }

    #[test]
    fn parses_explicit_json_column() {
        let expression = parse_match_expression(r#"#json.extra("$.tags")=important"#).unwrap();
        assert!(
            matches!(expression, FindExpr::Json(predicate) if predicate.column.as_deref() == Some("extra"))
        );
        let expression = parse_match_expression(r#"#json.metrics("$.score")>0"#).unwrap();
        assert!(expression.has_step_json());
    }

    #[test]
    fn rejects_unknown_field_and_malformed_json() {
        assert!(parse_match_expression("#unknown(value)").is_err());
        assert!(parse_match_expression("$.tags").is_err());
        assert!(parse_match_expression("#json($.tags)").is_err());
    }
}
