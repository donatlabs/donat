use donat_sqlgen::rule_qualified_column;

#[test]
fn rules_sql_columns_quote_each_identifier_component() {
    assert_eq!(
        rule_qualified_column("input\" alias", "amount\"; DROP TABLE users; --"),
        "\"input\"\" alias\".\"amount\"\"; DROP TABLE users; --\""
    );
}
