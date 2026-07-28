use donat_rules::{
    BinaryOp, Expr, ExprKind, Function, Literal, ParseError, UnaryOp, parse_expression,
};

const RULE: &str = "checkout_policy";

fn parse(source: &str) -> Expr {
    parse_expression(RULE, source).expect("expression should parse")
}

fn reject(source: &str) -> ParseError {
    parse_expression(RULE, source).expect_err("expression should be rejected")
}

fn name(expr: &Expr) -> &str {
    match &expr.kind {
        ExprKind::Name(name) => name,
        other => panic!("expected name expression, got {other:?}"),
    }
}

fn string(expr: &Expr) -> &str {
    match &expr.kind {
        ExprKind::Literal(Literal::String(value)) => value,
        other => panic!("expected string literal, got {other:?}"),
    }
}

#[test]
fn parses_scalar_and_list_literals() {
    let null = parse("null");
    assert!(matches!(null.kind, ExprKind::Literal(Literal::Null)));

    let boolean = parse("false");
    assert!(matches!(
        boolean.kind,
        ExprKind::Literal(Literal::Bool(false))
    ));

    let integer = parse("42");
    assert!(matches!(
        integer.kind,
        ExprKind::Literal(Literal::Int(ref value)) if value == "42"
    ));

    let decimal = parse("3.140");
    assert!(matches!(
        decimal.kind,
        ExprKind::Literal(Literal::Decimal(ref value)) if value == "3.140"
    ));

    let text = parse("'it\\'s ready'");
    assert!(matches!(
        text.kind,
        ExprKind::Literal(Literal::String(ref value)) if value == "it's ready"
    ));

    let list = parse("[1, true, null]");
    let ExprKind::List(items) = list.kind else {
        panic!("expected list literal");
    };
    assert_eq!(items.len(), 3);
    assert!(matches!(items[0].kind, ExprKind::Literal(Literal::Int(ref value)) if value == "1"));
    assert!(matches!(
        items[1].kind,
        ExprKind::Literal(Literal::Bool(true))
    ));
    assert!(matches!(items[2].kind, ExprKind::Literal(Literal::Null)));
}

#[test]
fn parses_static_object_and_list_member_access() {
    let expression = parse("customer.addresses[0].country");

    let ExprKind::Member { target, field } = expression.kind else {
        panic!("expected terminal object member access");
    };
    assert_eq!(field, "country");

    let ExprKind::Index { target, index } = target.kind else {
        panic!("expected literal list index");
    };
    assert_eq!(index, 0);

    let ExprKind::Member { target, field } = target.kind else {
        panic!("expected object member before index");
    };
    assert_eq!(field, "addresses");
    assert_eq!(name(&target), "customer");
}

#[test]
fn parses_boolean_precedence_deterministically() {
    let expression = parse("a || b && !c");

    let ExprKind::Binary { op, left, right } = expression.kind else {
        panic!("expected boolean binary expression");
    };
    assert_eq!(op, BinaryOp::Or);
    assert_eq!(name(&left), "a");

    let ExprKind::Binary { op, left, right } = right.kind else {
        panic!("expected && on right side of ||");
    };
    assert_eq!(op, BinaryOp::And);
    assert_eq!(name(&left), "b");

    let ExprKind::Unary { op, operand } = right.kind else {
        panic!("expected ! to bind before &&");
    };
    assert_eq!(op, UnaryOp::Not);
    assert_eq!(name(&operand), "c");
}

#[test]
fn parses_arithmetic_before_comparisons() {
    let expression = parse("subtotal + tax * quantity >= discount - 5");

    let ExprKind::Binary { op, left, right } = expression.kind else {
        panic!("expected comparison");
    };
    assert_eq!(op, BinaryOp::GreaterThanOrEqual);

    let ExprKind::Binary {
        op,
        left: sum_left,
        right: sum_right,
    } = left.kind
    else {
        panic!("expected additive expression on comparison left");
    };
    assert_eq!(op, BinaryOp::Add);
    assert_eq!(name(&sum_left), "subtotal");
    let ExprKind::Binary {
        op,
        left: product_left,
        right: product_right,
    } = sum_right.kind
    else {
        panic!("expected multiplication before addition");
    };
    assert_eq!(op, BinaryOp::Multiply);
    assert_eq!(name(&product_left), "tax");
    assert_eq!(name(&product_right), "quantity");

    let ExprKind::Binary {
        op,
        left: difference_left,
        right: difference_right,
    } = right.kind
    else {
        panic!("expected subtraction on comparison right");
    };
    assert_eq!(op, BinaryOp::Subtract);
    assert_eq!(name(&difference_left), "discount");
    assert!(matches!(
        difference_right.kind,
        ExprKind::Literal(Literal::Int(ref value)) if value == "5"
    ));
}

#[test]
fn parses_each_comparison_and_remaining_arithmetic_operators() {
    let comparisons = [
        ("left == right", BinaryOp::Equal),
        ("left != right", BinaryOp::NotEqual),
        ("left < right", BinaryOp::LessThan),
        ("left <= right", BinaryOp::LessThanOrEqual),
        ("left > right", BinaryOp::GreaterThan),
        ("left >= right", BinaryOp::GreaterThanOrEqual),
    ];
    for (source, expected_operator) in comparisons {
        let expression = parse(source);
        let ExprKind::Binary { op, .. } = expression.kind else {
            panic!("expected binary expression for {source}");
        };
        assert_eq!(
            op, expected_operator,
            "wrong comparison operator for {source}"
        );
    }

    let division = parse("total / quantity");
    assert!(matches!(
        division.kind,
        ExprKind::Binary {
            op: BinaryOp::Divide,
            ..
        }
    ));

    let negation = parse("-amount");
    assert!(matches!(
        negation.kind,
        ExprKind::Unary {
            op: UnaryOp::Negate,
            ..
        }
    ));
}

#[test]
fn retains_utf8_byte_spans_for_deploy_time_diagnostics() {
    let source = "startsWith(name, 'é')";
    let expression = parse(source);

    assert_eq!(expression.span.start, 0);
    assert_eq!(expression.span.end, source.len());
    let ExprKind::Call { arguments, .. } = expression.kind else {
        panic!("expected function call");
    };
    assert_eq!(arguments[1].span.start, "startsWith(name, ".len());
    assert_eq!(arguments[1].span.end, source.len() - 1);
}

#[test]
fn parses_ternary_after_boolean_expression() {
    let expression = parse("paid && !refunded ? 'complete' : 'pending'");

    let ExprKind::Conditional {
        condition,
        when_true,
        when_false,
    } = expression.kind
    else {
        panic!("expected ternary expression");
    };
    let ExprKind::Binary { op, .. } = condition.kind else {
        panic!("expected boolean condition");
    };
    assert_eq!(op, BinaryOp::And);
    assert_eq!(string(&when_true), "complete");
    assert_eq!(string(&when_false), "pending");
}

#[test]
fn parses_each_allowed_function() {
    let cases = [
        ("size(lines)", Function::Size, 1),
        ("is_null(customer.email)", Function::IsNull, 1),
        ("startsWith(customer.name, 'A')", Function::StartsWith, 2),
        ("endsWith(customer.name, \"z\")", Function::EndsWith, 2),
    ];

    for (source, function, argument_count) in cases {
        let expression = parse(source);
        let ExprKind::Call {
            function: parsed_function,
            arguments,
        } = expression.kind
        else {
            panic!("expected function call for {source}");
        };
        assert_eq!(parsed_function, function, "wrong function for {source}");
        assert_eq!(arguments.len(), argument_count, "wrong arity for {source}");
    }
}

#[test]
fn parses_nominal_enum_symbols_with_the_declaring_type_name() {
    let expression = parse("OrderStatus::draft");

    assert!(matches!(
        expression.kind,
        ExprKind::EnumSymbol { ref enum_name, ref symbol }
            if enum_name == "OrderStatus" && symbol == "draft"
    ));
}

#[test]
fn rejects_unknown_syntax_with_rule_and_offset() {
    let error = reject("a @ b");

    assert_eq!(error.rule_name, RULE);
    assert_eq!(error.offset, 2);
    assert_eq!(error.expectation, "a supported expression token");
}

#[test]
fn rejects_unknown_function_before_type_checking() {
    let error = reject("contains(customer.name, 'A')");

    assert_eq!(error.rule_name, RULE);
    assert_eq!(error.offset, 0);
    assert_eq!(
        error.expectation,
        "one of size, is_null, startsWith, endsWith"
    );
}

#[test]
fn rejects_input_longer_than_four_kib() {
    let error = reject(&"x".repeat(4097));

    assert_eq!(error.rule_name, RULE);
    assert_eq!(error.offset, 4096);
    assert_eq!(error.expectation, "an expression of at most 4096 bytes");
}

#[test]
fn accepts_inclusive_expression_limits() {
    let source_at_byte_limit = "x".repeat(4096);
    let expression = parse(&source_at_byte_limit);
    assert_eq!(expression.span.end, 4096);

    let expression_at_ast_depth_limit = parse(&format!("{}true", "!".repeat(63)));
    assert!(matches!(
        expression_at_ast_depth_limit.kind,
        ExprKind::Unary {
            op: UnaryOp::Not,
            ..
        }
    ));

    let list_at_item_limit = parse(&format!(
        "[{}]",
        std::iter::repeat_n("0", 256).collect::<Vec<_>>().join(",")
    ));
    let ExprKind::List(items) = list_at_item_limit.kind else {
        panic!("expected list literal at the item limit");
    };
    assert_eq!(items.len(), 256);
}

#[test]
fn accepts_sixty_four_nested_groups() {
    let expression = parse(&format!("{}true{}", "(".repeat(64), ")".repeat(64)));

    assert!(matches!(
        expression.kind,
        ExprKind::Literal(Literal::Bool(true))
    ));
}

#[test]
fn rejects_grouped_expression_depth_greater_than_sixty_four() {
    let error = reject(&format!("{}true{}", "(".repeat(65), ")".repeat(65)));

    assert_eq!(error.rule_name, RULE);
    assert_eq!(error.offset, 64);
    assert_eq!(
        error.expectation,
        "an expression nesting depth of at most 64"
    );
}

#[test]
fn rejects_ast_depth_greater_than_sixty_four() {
    let error = reject(&format!("{}true", "!".repeat(64)));

    assert_eq!(error.rule_name, RULE);
    assert_eq!(error.offset, 0);
    assert_eq!(
        error.expectation,
        "an expression nesting depth of at most 64"
    );
}

#[test]
fn rejects_list_literals_with_more_than_two_hundred_fifty_six_items() {
    let source = format!(
        "[{}]",
        std::iter::repeat_n("0", 257).collect::<Vec<_>>().join(",")
    );
    let error = reject(&source);

    assert_eq!(error.rule_name, RULE);
    assert_eq!(error.offset, 513);
    assert_eq!(error.expectation, "a list literal with at most 256 items");
}
