use noirc_frontend::{
    hir::resolution::errors::Span, ArrayLiteral, BlockExpression, Expression, ExpressionKind,
    Literal, Statement,
};

use super::FmtVisitor;

impl FmtVisitor<'_> {
    pub(crate) fn visit_expr(&mut self, expr: Expression) {
        let span = expr.span;

        let rewrite = self.format_expr(expr);
        self.push_rewrite(rewrite, span);

        self.last_position = span.end();
    }

    fn format_expr(&self, Expression { kind, span }: Expression) -> String {
        match kind {
            ExpressionKind::Block(block) => {
                let mut visitor = FmtVisitor::new(self.source, self.config);

                visitor.block_indent = self.block_indent;
                visitor.visit_block(block, span, true);

                visitor.buffer
            }
            ExpressionKind::Prefix(prefix) => {
                format!("{}{}", prefix.operator, self.format_expr(prefix.rhs))
            }
            ExpressionKind::Cast(cast) => {
                format!("{} as {}", self.format_expr(cast.lhs), cast.r#type)
            }
            ExpressionKind::Infix(infix) => {
                format!(
                    "{} {} {}",
                    self.format_expr(infix.lhs),
                    infix.operator.contents.as_string(),
                    self.format_expr(infix.rhs)
                )
            }
            ExpressionKind::Index(index_expr) => {
                let formatted_collection = self.format_expr(index_expr.collection);
                let formatted_index = self.format_expr(index_expr.index);
                format!("{}[{}]", formatted_collection, formatted_index)
            }
            ExpressionKind::Literal(literal) => match literal {
                Literal::Integer(_) => slice!(self, span.start(), span.end()).to_string(),
                Literal::Array(ArrayLiteral::Repeated { repeated_element, length }) => {
                    format!("[{}; {length}]", self.format_expr(*repeated_element))
                }
                // TODO: Handle line breaks when array gets too long.
                Literal::Array(ArrayLiteral::Standard(exprs)) => {
                    let contents: Vec<String> =
                        exprs.into_iter().map(|expr| self.format_expr(expr)).collect();
                    format!("[{}]", contents.join(", "))
                }

                Literal::Bool(_) | Literal::Str(_) | Literal::FmtStr(_) | Literal::Unit => {
                    literal.to_string()
                }
            }
            ExpressionKind::Call(call_expr) => {
                let formatted_func = self.format_expr(*call_expr.func);
                let formatted_args = call_expr.arguments
                    .iter()
                    .map(|arg| self.format_expr(arg.clone()))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{}({})", formatted_func, formatted_args)
            }
            ExpressionKind::MethodCall(method_call_expr) => {
                let formatted_object = self.format_expr(method_call_expr.object);
                let formatted_args = method_call_expr.arguments
                    .iter()
                    .map(|arg| self.format_expr(arg.clone()))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{}.{}({})", formatted_object, method_call_expr.method_name, formatted_args)
            }
            ExpressionKind::Constructor(constructor_expr) => {
                let type_str = constructor_expr.type_name.to_string();
                let formatted_fields = constructor_expr.fields
                    .iter()
                    .map(|(field_ident, field_value)| format!("{}: {}", field_ident, self.format_expr(field_value.clone())))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{} {{ {} }}", type_str, formatted_fields)
            }
            ExpressionKind::MemberAccess(member_access_expr) => {
                let lhs_str = self.format_expr(member_access_expr.lhs);
                format!("{}.{}", lhs_str, member_access_expr.rhs)
            }
            ExpressionKind::Infix(infix_expr) => {
                let lhs_str = self.format_expr(infix_expr.lhs);
                let rhs_str = self.format_expr(infix_expr.rhs);
                format!("{} {} {}", lhs_str, infix_expr.operator, rhs_str)
            }
            ExpressionKind::If(if_expr) => {
                let condition_str = self.format_expr(if_expr.condition);
                let consequence_str = self.format_expr(if_expr.consequence);
                
                if let Some(alternative_expr) = &if_expr.alternative {
                    let alternative_str = self.format_expr(alternative_expr.clone());
                    format!("if {} {{ {} }} else {{ {} }}", condition_str, consequence_str, alternative_str)
                } else {
                    format!("if {} {{ {} }}", condition_str, consequence_str)
                }
            }
            ExpressionKind::Variable(path) => path.to_string()
            ExpressionKind::Lambda(lambda) => {
                let formatted_params = lambda.params
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                let formatted_body = self.format_expr(*lambda.body);
                format!("|{}| -> {}", formatted_params, formatted_body)
            }
            ExpressionKind::Tuple(elements) => {
                let formatted_elements = elements
                    .iter()
                    .map(|e| self.format_expr(e.clone()))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("({})", formatted_elements)
            }
            // TODO:
            _expr => slice!(self, span.start(), span.end()).to_string(),
        }
    }

    pub(crate) fn visit_block(
        &mut self,
        block: BlockExpression,
        block_span: Span,
        should_indent: bool,
    ) {
        if block.is_empty() {
            self.visit_empty_block(block_span, should_indent);
            return;
        }

        self.last_position = block_span.start() + 1; // `{`
        self.push_str("{");

        self.trim_spaces_after_opening_brace(&block.0);

        self.with_indent(|this| {
            this.visit_stmts(block.0);
        });

        let slice = slice!(self, self.last_position, block_span.end() - 1).trim_end();
        self.push_str(slice);

        self.last_position = block_span.end();

        self.push_str("\n");
        if should_indent {
            self.push_str(&self.block_indent.to_string());
        }
        self.push_str("}");
    }

    fn trim_spaces_after_opening_brace(&mut self, block: &[Statement]) {
        if let Some(first_stmt) = block.first() {
            let slice = slice!(self, self.last_position, first_stmt.span.start());
            let len =
                slice.chars().take_while(|ch| ch.is_whitespace()).collect::<String>().rfind('\n');
            self.last_position += len.unwrap_or(0) as u32;
        }
    }

    fn visit_empty_block(&mut self, block_span: Span, should_indent: bool) {
        let slice = slice!(self, block_span.start(), block_span.end());
        let comment_str = slice[1..slice.len() - 1].trim();
        let block_str = if comment_str.is_empty() {
            "{}".to_string()
        } else {
            self.block_indent.block_indent(self.config);
            let open_indent = self.block_indent.to_string();
            self.block_indent.block_unindent(self.config);
            let close_indent =
                if should_indent { self.block_indent.to_string() } else { String::new() };

            let ret = format!("{{\n{open_indent}{comment_str}\n{close_indent}}}");
            ret
        };
        self.last_position = block_span.end();
        self.push_str(&block_str);
    }
}
