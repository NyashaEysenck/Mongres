use std::collections::BTreeSet;

use mongo_pg_common::ErrorKind;
use mongo_pg_schema_discovery::{FieldPath, FieldProfile, ObservedShape, ObservedType};

use crate::{ComparisonOperator, Predicate, Projection, SqlValue, StatementPlan, parse_sql};

fn schema() -> mongo_pg_schema_discovery::SchemaProfile {
    let fields = [
        (FieldPath::top_level("name"), ObservedType::String),
        (FieldPath::top_level("active"), ObservedType::Boolean),
        (FieldPath::top_level("status"), ObservedType::Integer),
        (
            FieldPath::top_level("profile").child("city"),
            ObservedType::String,
        ),
    ]
    .into_iter()
    .map(|(path, observed_type)| FieldProfile {
        path,
        present_documents: 2,
        missing_documents: 0,
        observed_types: BTreeSet::from([observed_type]),
        observed_shapes: BTreeSet::from([ObservedShape::Scalar]),
        has_dotted_key_collision: false,
    })
    .collect();
    mongo_pg_schema_discovery::SchemaProfile {
        profile_version: 1,
        sampled_documents: 2,
        fields,
    }
}

#[test]
fn lowers_a_nested_select_predicate_to_a_typed_plan() {
    let plan = parse_sql(
        "SELECT name, profile.city FROM customers WHERE active = true AND status IN (1, 2) LIMIT 5",
        "customers",
        &schema(),
    )
    .expect("SELECT should parse");

    let StatementPlan::Select(plan) = plan else {
        panic!("expected SELECT plan");
    };
    assert_eq!(plan.limit, Some(5));
    assert!(matches!(plan.projection, Projection::Fields(fields) if fields.len() == 2));
    assert!(matches!(
        plan.filter,
        Some(Predicate::And(predicates)) if predicates.len() == 2
    ));
}

#[test]
fn lowers_insert_update_and_delete_with_typed_values() {
    let insert = parse_sql(
        "INSERT INTO customers (name, active) VALUES ('Amina', true), ('Tendai', false)",
        "customers",
        &schema(),
    )
    .expect("INSERT should parse");
    assert!(matches!(
        insert,
        StatementPlan::Insert(plan) if plan.rows.len() == 2 && plan.rows[0][0] == SqlValue::String("Amina".into())
    ));

    let nested_insert = parse_sql(
        "INSERT INTO customers (name, \"profile.city\") VALUES ('Amina', 'Harare')",
        "customers",
        &schema(),
    )
    .expect("quoted nested INSERT column should parse");
    assert!(matches!(
        nested_insert,
        StatementPlan::Insert(plan) if plan.columns[1] == FieldPath::top_level("profile").child("city")
    ));

    let update = parse_sql(
        "UPDATE customers SET profile.city = 'Bulawayo' WHERE name = 'Amina'",
        "customers",
        &schema(),
    )
    .expect("UPDATE should parse");
    assert!(matches!(
        update,
        StatementPlan::Update(plan)
            if plan.assignments.len() == 1
                && matches!(plan.filter, Predicate::Compare { operator: ComparisonOperator::Equal, .. })
    ));

    let delete = parse_sql(
        "DELETE FROM customers WHERE status >= 2",
        "customers",
        &schema(),
    )
    .expect("DELETE should parse");
    assert!(matches!(delete, StatementPlan::Delete(_)));
}

#[test]
fn rejects_unsafe_or_unsupported_sql_before_execution() {
    let join_error = parse_sql(
        "SELECT * FROM customers JOIN orders ON customers.name = orders.customer",
        "customers",
        &schema(),
    )
    .expect_err("JOIN should be rejected");
    assert_eq!(join_error.kind, ErrorKind::FeatureNotSupported);

    let unfiltered_error = parse_sql("DELETE FROM customers", "customers", &schema())
        .expect_err("unfiltered DELETE should be rejected");
    assert_eq!(unfiltered_error.kind, ErrorKind::InvalidInput);

    let unknown_field_error = parse_sql(
        "SELECT unknown_field FROM customers",
        "customers",
        &schema(),
    )
    .expect_err("unknown field should be rejected");
    assert_eq!(unknown_field_error.kind, ErrorKind::InvalidInput);
}
