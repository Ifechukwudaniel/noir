use acvm::AcirField;
use noirc_errors::Span;

use crate::{
    ast::{
        BlockExpression, FunctionDefinition, FunctionReturnType, Ident, ItemVisibility,
        NoirFunction, Param, UnresolvedType, UnresolvedTypeData, Visibility,
    },
    parser::ParserErrorReason,
    token::{Attribute, Attributes, Keyword, Token},
};

use super::Parser;

impl<'a> Parser<'a> {
    pub(crate) fn parse_function(
        &mut self,
        attributes: Vec<(Attribute, Span)>,
        visibility: ItemVisibility,
        is_comptime: bool,
        is_unconstrained: bool,
        allow_self: bool,
        start_span: Span,
    ) -> NoirFunction {
        self.parse_function_definition(
            attributes,
            visibility,
            is_comptime,
            is_unconstrained,
            allow_self,
            start_span,
        )
        .into()
    }

    pub(crate) fn parse_function_definition(
        &mut self,
        attributes: Vec<(Attribute, Span)>,
        visibility: ItemVisibility,
        is_comptime: bool,
        is_unconstrained: bool,
        allow_self: bool,
        start_span: Span,
    ) -> FunctionDefinition {
        let attributes = self.validate_attributes(attributes);

        let Some(name) = self.eat_ident() else {
            // TODO: error
            return empty_function(
                attributes,
                is_unconstrained,
                is_comptime,
                visibility,
                start_span,
            );
        };

        let generics = self.parse_generics();
        let parameters = self.parse_function_parameters(allow_self);

        let (return_type, return_visibility) = if self.eat(Token::Arrow) {
            let visibility = self.parse_visibility();
            (FunctionReturnType::Ty(self.parse_type()), visibility)
        } else {
            (FunctionReturnType::Default(Span::default()), Visibility::Private)
        };

        let where_clause = self.parse_where_clause();

        let body = self.parse_block_expression().unwrap_or_else(|| empty_body());

        FunctionDefinition {
            name,
            attributes,
            is_unconstrained,
            is_comptime,
            visibility,
            generics,
            parameters,
            body,
            span: start_span,
            where_clause,
            return_type,
            return_visibility,
        }
    }

    fn parse_function_parameters(&mut self, allow_self: bool) -> Vec<Param> {
        let mut parameters = Vec::new();

        if !self.eat_left_paren() {
            return parameters;
        }

        loop {
            if self.eat_right_paren() {
                break;
            }

            let start_span = self.current_token_span;
            let pattern = self.parse_pattern();
            if self.current_token_span == start_span {
                // TODO: error
                self.eat_right_paren();
                break;
            }

            if self.eat_colon() {
                let visibility = self.parse_visibility();

                let typ = self.parse_type();
                parameters.push(Param {
                    visibility,
                    pattern,
                    typ,
                    span: self.span_since(start_span),
                });
            } else {
                // TODO: error
                parameters.push(Param {
                    visibility: Visibility::Private,
                    pattern,
                    typ: UnresolvedType { typ: UnresolvedTypeData::Error, span: Span::default() },
                    span: self.span_since(start_span),
                });
            }

            self.eat_commas();
            // TODO: error if no commas between parameters
        }

        parameters
    }

    fn parse_visibility(&mut self) -> Visibility {
        if self.eat_keyword(Keyword::Pub) {
            return Visibility::Public;
        }

        if self.eat_keyword(Keyword::ReturnData) {
            return Visibility::ReturnData;
        }

        if self.eat_keyword(Keyword::CallData) {
            if self.eat_left_paren() {
                if let Some(int) = self.eat_int() {
                    if !self.eat_right_paren() {
                        // TODO: error
                    }

                    let id = int.to_u128() as u32;
                    return Visibility::CallData(id);
                } else {
                    // TODO: error
                    if !self.eat_right_paren() {
                        // TODO: error
                    }
                    return Visibility::CallData(0);
                }
            } else {
                // TODO: error
                return Visibility::CallData(0);
            }
        }

        Visibility::Private
    }

    fn validate_attributes(&mut self, attributes: Vec<(Attribute, Span)>) -> Attributes {
        let mut primary = None;
        let mut secondary = Vec::new();

        for (attribute, span) in attributes {
            match attribute {
                Attribute::Function(attr) => {
                    if primary.is_some() {
                        self.push_error(ParserErrorReason::MultipleFunctionAttributesFound, span);
                    }
                    primary = Some(attr);
                }
                Attribute::Secondary(attr) => secondary.push(attr),
            }
        }

        Attributes { function: primary, secondary }
    }
}

fn empty_function(
    attributes: Attributes,
    is_unconstrained: bool,
    is_comptime: bool,
    visibility: ItemVisibility,
    start_span: Span,
) -> FunctionDefinition {
    FunctionDefinition {
        name: Ident::default(),
        attributes,
        is_unconstrained,
        is_comptime,
        visibility,
        generics: Vec::new(),
        parameters: Vec::new(),
        body: empty_body(),
        span: start_span,
        where_clause: Vec::new(),
        return_type: FunctionReturnType::Default(Span::default()),
        return_visibility: Visibility::Private,
    }
}

fn empty_body() -> BlockExpression {
    BlockExpression { statements: Vec::new() }
}

#[cfg(test)]
mod tests {
    use crate::{
        ast::{UnresolvedTypeData, Visibility},
        parser::{parser::parse_program, ItemKind, ParserErrorReason},
    };

    #[test]
    fn parse_simple_function() {
        let src = "fn foo() {}";
        let (module, errors) = parse_program(src);
        assert!(errors.is_empty());
        assert_eq!(module.items.len(), 1);
        let item = &module.items[0];
        let ItemKind::Function(noir_function) = &item.kind else {
            panic!("Expected function");
        };
        assert_eq!("foo", noir_function.def.name.to_string());
        assert!(noir_function.def.parameters.is_empty());
        assert!(noir_function.def.generics.is_empty());
    }

    #[test]
    fn parse_function_with_generics() {
        let src = "fn foo<A>() {}";
        let (module, errors) = parse_program(src);
        assert!(errors.is_empty());
        assert_eq!(module.items.len(), 1);
        let item = &module.items[0];
        let ItemKind::Function(noir_function) = &item.kind else {
            panic!("Expected function");
        };
        assert_eq!(noir_function.def.generics.len(), 1);
    }

    #[test]
    fn parse_function_with_arguments() {
        let src = "fn foo(x: Field, y: Field) {}";
        let (mut module, errors) = parse_program(src);
        assert!(errors.is_empty());
        assert_eq!(module.items.len(), 1);
        let item = module.items.remove(0);
        let ItemKind::Function(mut noir_function) = item.kind else {
            panic!("Expected function");
        };
        assert_eq!(noir_function.def.parameters.len(), 2);

        let param = noir_function.def.parameters.remove(0);
        assert_eq!("x", param.pattern.to_string());
        assert_eq!("Field", param.typ.to_string());
        assert_eq!(param.visibility, Visibility::Private);

        let param = noir_function.def.parameters.remove(0);
        assert_eq!("y", param.pattern.to_string());
        assert_eq!("Field", param.typ.to_string());
        assert_eq!(param.visibility, Visibility::Private);
    }

    #[test]
    fn parse_function_with_argument_pub_visibility() {
        let src = "fn foo(x: pub Field) {}";
        let (mut module, errors) = parse_program(src);
        assert!(errors.is_empty());
        assert_eq!(module.items.len(), 1);
        let item = module.items.remove(0);
        let ItemKind::Function(mut noir_function) = item.kind else {
            panic!("Expected function");
        };
        assert_eq!(noir_function.def.parameters.len(), 1);

        let param = noir_function.def.parameters.remove(0);
        assert_eq!("x", param.pattern.to_string());
        assert_eq!("Field", param.typ.to_string());
        assert_eq!(param.visibility, Visibility::Public);
    }

    #[test]
    fn parse_function_with_argument_return_data_visibility() {
        let src = "fn foo(x: return_data Field) {}";
        let (mut module, errors) = parse_program(src);
        assert!(errors.is_empty());
        assert_eq!(module.items.len(), 1);
        let item = module.items.remove(0);
        let ItemKind::Function(mut noir_function) = item.kind else {
            panic!("Expected function");
        };
        assert_eq!(noir_function.def.parameters.len(), 1);

        let param = noir_function.def.parameters.remove(0);
        assert_eq!(param.visibility, Visibility::ReturnData);
    }

    #[test]
    fn parse_function_with_argument_call_data_visibility() {
        let src = "fn foo(x: call_data(42) Field) {}";
        let (mut module, errors) = parse_program(src);
        assert!(errors.is_empty());
        assert_eq!(module.items.len(), 1);
        let item = module.items.remove(0);
        let ItemKind::Function(mut noir_function) = item.kind else {
            panic!("Expected function");
        };
        assert_eq!(noir_function.def.parameters.len(), 1);

        let param = noir_function.def.parameters.remove(0);
        assert_eq!(param.visibility, Visibility::CallData(42));
    }

    #[test]
    fn parse_function_return_type() {
        let src = "fn foo() -> Field {}";
        let (module, errors) = parse_program(src);
        assert!(errors.is_empty());
        assert_eq!(module.items.len(), 1);
        let item = &module.items[0];
        let ItemKind::Function(noir_function) = &item.kind else {
            panic!("Expected function");
        };
        assert_eq!(noir_function.def.return_visibility, Visibility::Private);
        assert_eq!(noir_function.return_type().typ, UnresolvedTypeData::FieldElement);
    }

    #[test]
    fn parse_function_return_visibility() {
        let src = "fn foo() -> pub Field {}";
        let (module, errors) = parse_program(src);
        assert!(errors.is_empty());
        assert_eq!(module.items.len(), 1);
        let item = &module.items[0];
        let ItemKind::Function(noir_function) = &item.kind else {
            panic!("Expected function");
        };
        assert_eq!(noir_function.def.return_visibility, Visibility::Public);
        assert_eq!(noir_function.return_type().typ, UnresolvedTypeData::FieldElement);
    }

    #[test]
    fn parse_function_unclosed_parentheses() {
        let src = "fn foo(x: i32,";
        let (module, errors) = parse_program(src);
        assert!(errors.is_empty()); // TODO: there should be errors here
        assert_eq!(module.items.len(), 1);
        let item = &module.items[0];
        let ItemKind::Function(noir_function) = &item.kind else {
            panic!("Expected function");
        };
        assert_eq!("foo", noir_function.def.name.to_string());
    }

    #[test]
    fn parse_error_multiple_function_attributes_found() {
        let src = "#[foreign(foo)] #[oracle(bar)] fn foo() {}";
        let (_, errors) = parse_program(src);
        assert_eq!(errors.len(), 1);

        let reason = errors[0].reason().unwrap();
        assert!(matches!(reason, ParserErrorReason::MultipleFunctionAttributesFound));
    }
}
