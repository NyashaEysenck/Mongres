use std::collections::{BTreeMap, BTreeSet};

use mongo_pg_common::ErrorKind;
use mongo_pg_schema_discovery::{FieldPath, FieldProfile, ObservedShape, ObservedType};

use crate::{
    ComparisonOperator, Predicate, Projection, SqlValue, StatementPlan, bind_parameters, parse_sql,
    parse_sql_for_profiles,
};

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

fn orders_schema() -> mongo_pg_schema_discovery::SchemaProfile {
    let fields = [
        (FieldPath::top_level("order_total"), ObservedType::Integer),
        (FieldPath::top_level("reference"), ObservedType::String),
    ]
    .into_iter()
    .map(|(path, observed_type)| FieldProfile {
        path,
        present_documents: 1,
        missing_documents: 0,
        observed_types: BTreeSet::from([observed_type]),
        observed_shapes: BTreeSet::from([ObservedShape::Scalar]),
        has_dotted_key_collision: false,
    })
    .collect();
    mongo_pg_schema_discovery::SchemaProfile {
        profile_version: 1,
        sampled_documents: 1,
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

#[test]
fn rejects_null_comparisons_that_mongodb_cannot_translate_with_sql_semantics() {
    for sql in [
        "SELECT name FROM customers WHERE name = NULL",
        "SELECT name FROM customers WHERE status IN (1, NULL)",
    ] {
        let error = parse_sql(sql, "customers", &schema())
            .expect_err("NULL comparisons must require IS NULL syntax");
        assert_eq!(error.kind, ErrorKind::InvalidInput);
    }
}

#[test]
fn enforces_exact_inferred_types_for_write_values_and_filters() {
    let assignment_error = parse_sql(
        "UPDATE customers SET active = 'true' WHERE name = 'Amina'",
        "customers",
        &schema(),
    )
    .expect_err("string values must not coerce to BSON booleans");
    assert_eq!(assignment_error.kind, ErrorKind::InvalidInput);

    let filter_error = parse_sql(
        "DELETE FROM customers WHERE status = '2'",
        "customers",
        &schema(),
    )
    .expect_err("string values must not coerce to BSON integers");
    assert_eq!(filter_error.kind, ErrorKind::InvalidInput);

    parse_sql(
        "UPDATE customers SET profile.city = NULL WHERE name = 'Amina'",
        "customers",
        &schema(),
    )
    .expect("SQL NULL is a valid assignment value");
}

#[test]
fn preserves_ambiguous_write_fields_for_the_policy_layer() {
    let mut ambiguous_schema = schema();
    let status = ambiguous_schema
        .fields
        .iter_mut()
        .find(|field| field.path == FieldPath::top_level("status"))
        .expect("status field must exist in fixture");
    status.observed_types.insert(ObservedType::String);

    let plan = parse_sql(
        "UPDATE customers SET status = 2 WHERE name = 'Amina'",
        "customers",
        &ambiguous_schema,
    )
    .expect("the policy layer must receive the typed ambiguous write plan");
    assert!(matches!(plan, StatementPlan::Update(_)));
}

#[test]
fn binds_postgresql_placeholders_and_revalidates_the_completed_plan() {
    let plan = parse_sql(
        "UPDATE customers SET active = $1 WHERE status = $2",
        "customers",
        &schema(),
    )
    .expect("placeholder plan should parse");

    let bound = bind_parameters(
        plan,
        &[SqlValue::Boolean(false), SqlValue::Integer(2)],
        &schema(),
    )
    .expect("typed parameters should bind");

    assert!(matches!(
        bound,
        StatementPlan::Update(plan)
            if plan.assignments[0].value == SqlValue::Boolean(false)
                && matches!(plan.filter, Predicate::Compare { value: SqlValue::Integer(2), .. })
    ));
}

#[test]
fn rejects_missing_extra_or_schema_incompatible_bound_parameters() {
    let plan = parse_sql(
        "UPDATE customers SET active = $1 WHERE status = $2",
        "customers",
        &schema(),
    )
    .expect("placeholder plan should parse");
    let missing = bind_parameters(plan.clone(), &[SqlValue::Boolean(false)], &schema())
        .expect_err("all placeholder values are required");
    assert_eq!(missing.kind, ErrorKind::InvalidInput);

    let extra = bind_parameters(
        plan.clone(),
        &[
            SqlValue::Boolean(false),
            SqlValue::Integer(2),
            SqlValue::String("extra".to_owned()),
        ],
        &schema(),
    )
    .expect_err("extra parameter values are rejected");
    assert_eq!(extra.kind, ErrorKind::InvalidInput);

    let incompatible = bind_parameters(
        plan,
        &[SqlValue::String("false".to_owned()), SqlValue::Integer(2)],
        &schema(),
    )
    .expect_err("bound values must use inferred BSON types");
    assert_eq!(incompatible.kind, ErrorKind::InvalidInput);
}

#[test]
fn routes_statements_to_the_exact_allowlisted_collection_profile() {
    let profiles = BTreeMap::from([
        ("customers".to_owned(), schema()),
        ("orders".to_owned(), orders_schema()),
    ]);

    let plan = parse_sql_for_profiles(
        "UPDATE orders SET order_total = 42 WHERE reference = 'ORD-1'",
        &profiles,
    )
    .expect("orders statement should use orders profile");
    assert!(matches!(
        plan,
        StatementPlan::Update(plan)
            if plan.collection == "orders"
                && plan.assignments[0].path == FieldPath::top_level("order_total")
    ));

    let cross_collection_field = parse_sql_for_profiles("SELECT name FROM orders", &profiles)
        .expect_err("customers fields must not leak into orders");
    assert_eq!(cross_collection_field.kind, ErrorKind::InvalidInput);

    let unallowlisted = parse_sql_for_profiles("SELECT * FROM invoices", &profiles)
        .expect_err("unknown table must be rejected before lowering");
    assert_eq!(unallowlisted.kind, ErrorKind::InvalidInput);
}
