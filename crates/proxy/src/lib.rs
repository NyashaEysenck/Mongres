//! `PostgreSQL` wire-protocol boundary for the deterministic `MongoDB` proxy.
//!
//! The server keeps protocol concerns at the edge: SQL is lowered into typed
//! plans before the executor sees it, and every execution error is emitted as
//! a `PostgreSQL` SQLSTATE response.

use std::{
    collections::BTreeMap,
    env,
    fmt::Debug,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use futures_util::{Sink, StreamExt, stream};
use mongo_pg_ambiguity_policy::audit::AmbiguityAuditRecord;
use mongo_pg_catalog::{CollectionCatalog, DatabaseCatalog, SqlType, project_public_collections};
use mongo_pg_common::{ErrorKind, ProxyError};
use mongo_pg_mongo_executor::{
    WriteOutcome, deterministic_write_client, execute_select, execute_write,
};
use mongo_pg_resolver_client::{ResolverClient, ResolverClientConfig};
use mongo_pg_schema_discovery::{
    CollectionProfiles, FieldPath, ObservedShape, ObservedType, SchemaProfile,
    load_required_persisted_profiles, validated_collection_names,
};
use mongo_pg_sql_engine::{
    Predicate, Projection, SelectPlan, SqlValue, StatementPlan, bind_parameters,
    parse_sql_for_profiles,
};
use mongodb::{
    Database,
    bson::{Bson, Document},
};
use pgwire::{
    api::{
        ClientInfo, PgWireHandlerFactory, Type,
        copy::NoopCopyHandler,
        portal::{Format, Portal},
        query::{ExtendedQueryHandler, SimpleQueryHandler},
        results::{
            DataRowEncoder, DescribePortalResponse, DescribeStatementResponse, FieldFormat,
            FieldInfo, QueryResponse, Response, Tag,
        },
        stmt::{QueryParser, StoredStatement},
    },
    error::{ErrorInfo, PgWireError, PgWireResult},
    messages::PgWireBackendMessage,
    tokio::process_socket,
};
use tokio::net::TcpListener;

mod ambiguity;
mod auth;
mod parameters;

pub use auth::ProxyAuthConfig;
use auth::ProxyStartupHandler;

/// Runtime settings for an allowlisted collection-backed proxy instance.
#[derive(Debug, Clone, PartialEq)]
pub struct ProxyConfig {
    pub mongo_uri: String,
    pub database_name: String,
    /// `MongoDB` collections exposed as SQL tables by this proxy process.
    pub collection_names: Vec<String>,
    pub listen_address: String,
    pub resolver_url: String,
    pub resolver_timeout: Duration,
    pub resolver_minimum_confidence: f64,
    pub auth: ProxyAuthConfig,
}

impl ProxyConfig {
    /// Reads the proxy configuration from the documented environment variables.
    ///
    /// # Errors
    ///
    /// Returns an actionable error when a required `MongoDB` setting is absent.
    pub fn from_environment() -> Result<Self, ProxyError> {
        Ok(Self {
            mongo_uri: required_environment("MONGO_URI")?,
            database_name: required_environment("MONGO_DATABASE")?,
            collection_names: configured_collection_names()?,
            listen_address: env::var("PROXY_LISTEN_ADDR")
                .unwrap_or_else(|_| "127.0.0.1:5433".to_owned()),
            resolver_url: env::var("AMBIGUITY_RESOLVER_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:8000/v1/resolve".to_owned()),
            resolver_timeout: Duration::from_millis(optional_environment(
                "AMBIGUITY_RESOLVER_TIMEOUT_MS",
                5_000,
            )?),
            resolver_minimum_confidence: optional_confidence(
                "AMBIGUITY_RESOLVER_MIN_CONFIDENCE",
                0.8,
            )?,
            auth: ProxyAuthConfig::from_environment()?,
        })
    }
}

/// Starts the `PostgreSQL` wire server after loading its persisted schemas.
///
/// A schema profile is required so the proxy never invents field paths or
/// result types at query time. Run schema discovery before starting the proxy.
///
/// # Errors
///
/// Returns an error when `MongoDB` is unavailable, schema discovery has not
/// persisted profiles for every configured collection, or the listener cannot
/// be started.
pub async fn run_server(config: ProxyConfig) -> Result<(), ProxyError> {
    let client = deterministic_write_client(&config.mongo_uri).await?;
    let database = client.database(&config.database_name);
    let profiles = load_required_persisted_profiles(&database, &config.collection_names)
        .await
        .map_err(|error| dependency_error("load persisted schema profiles", &error))?;
    let listener = TcpListener::bind(&config.listen_address)
        .await
        .map_err(|error| dependency_error("bind PostgreSQL listener", &error))?;
    let resolver_config = ResolverClientConfig::new(&config.resolver_url, config.resolver_timeout);
    let resolver = ResolverClient::new(&resolver_config)?;
    let backend = Arc::new(ProxyBackend::new(
        database,
        config.database_name,
        profiles,
        resolver,
        config.resolver_minimum_confidence,
    ));
    let factory = Arc::new(ProxyHandlerFactory {
        backend,
        auth: config.auth,
    });

    loop {
        let (socket, _) = listener
            .accept()
            .await
            .map_err(|error| dependency_error("accept PostgreSQL client", &error))?;
        let factory = factory.clone();
        tokio::spawn(async move {
            if let Err(error) = process_socket(socket, None, factory).await {
                eprintln!("PostgreSQL client session ended with an error: {error}");
            }
        });
    }
}

fn configured_collection_names() -> Result<Vec<String>, ProxyError> {
    let configured_list = env::var("MONGO_COLLECTIONS").ok();
    let legacy_collection = match env::var("MONGO_COLLECTION") {
        Ok(value) => Some(value),
        Err(env::VarError::NotPresent) => None,
        Err(error) => return Err(ProxyError::new(ErrorKind::InvalidInput, error.to_string())),
    };
    parse_configured_collection_names(configured_list.as_deref(), legacy_collection.as_deref())
}

fn parse_configured_collection_names(
    configured_list: Option<&str>,
    legacy_collection: Option<&str>,
) -> Result<Vec<String>, ProxyError> {
    let collections = match configured_list.filter(|value| !value.trim().is_empty()) {
        Some(value) => value
            .split(',')
            .map(str::trim)
            .map(ToOwned::to_owned)
            .collect(),
        None => vec![legacy_collection
            .ok_or_else(|| {
                ProxyError::new(
                    ErrorKind::InvalidInput,
                    "required environment variable is missing: MONGO_COLLECTION or MONGO_COLLECTIONS",
                )
            })?
            .to_owned()],
    };
    validated_collection_names(&collections)
        .map_err(|error| dependency_error("validate configured MongoDB collections", &error))
}

fn is_catalog_or_session_query(query: &str) -> bool {
    let normalized = query.trim().trim_end_matches(';').to_ascii_lowercase();
    normalized == "select 1"
        || normalized.contains("current_database()")
        || normalized.contains("current_schema()")
        || normalized.contains("version()")
        || normalized.starts_with("show server_version")
        || normalized.contains("information_schema.tables")
        || normalized.contains("information_schema.columns")
        || normalized.contains("pg_catalog.pg_tables")
        || normalized.contains("pg_catalog.pg_class")
}

fn required_environment(name: &str) -> Result<String, ProxyError> {
    env::var(name).map_err(|_| {
        ProxyError::new(
            ErrorKind::InvalidInput,
            format!("required environment variable is missing: {name}"),
        )
    })
}

fn optional_environment(name: &str, default: u64) -> Result<u64, ProxyError> {
    match env::var(name) {
        Ok(value) => value.parse().map_err(|_| {
            ProxyError::new(
                ErrorKind::InvalidInput,
                format!("{name} must be a positive integer"),
            )
        }),
        Err(env::VarError::NotPresent) => Ok(default),
        Err(error) => Err(ProxyError::new(ErrorKind::InvalidInput, error.to_string())),
    }
}

fn optional_confidence(name: &str, default: f64) -> Result<f64, ProxyError> {
    let confidence = match env::var(name) {
        Ok(value) => value.parse().map_err(|_| {
            ProxyError::new(
                ErrorKind::InvalidInput,
                format!("{name} must be a number between zero and one"),
            )
        })?,
        Err(env::VarError::NotPresent) => default,
        Err(error) => return Err(ProxyError::new(ErrorKind::InvalidInput, error.to_string())),
    };
    if !confidence.is_finite() || !(0.0..=1.0).contains(&confidence) {
        return Err(ProxyError::new(
            ErrorKind::InvalidInput,
            format!("{name} must be a number between zero and one"),
        ));
    }
    Ok(confidence)
}

fn dependency_error(action: &str, error: &impl std::fmt::Display) -> ProxyError {
    ProxyError::new(
        ErrorKind::Dependency,
        format!("failed to {action}: {error}"),
    )
}

struct ProxyBackend {
    database: Database,
    database_name: String,
    profiles: CollectionProfiles,
    catalog: DatabaseCatalog,
    resolver: ResolverClient,
    resolver_minimum_confidence: f64,
    audit_records: Mutex<Vec<AmbiguityAuditRecord>>,
}

#[derive(Debug, Clone)]
struct PreparedStatement {
    sql: String,
    parameter_types: Vec<Type>,
}

#[derive(Debug)]
struct ProxyQueryParser {
    profiles: CollectionProfiles,
}

#[async_trait]
impl QueryParser for ProxyQueryParser {
    type Statement = PreparedStatement;

    async fn parse_sql(&self, sql: &str, types: &[Type]) -> PgWireResult<Self::Statement> {
        prepare_statement(sql, types, &self.profiles).map_err(proxy_error_to_pgwire)
    }
}

impl ProxyBackend {
    fn new(
        database: Database,
        database_name: String,
        profiles: CollectionProfiles,
        resolver: ResolverClient,
        resolver_minimum_confidence: f64,
    ) -> Self {
        let catalog = project_public_collections(&profiles);
        Self {
            database,
            database_name,
            profiles,
            catalog,
            resolver,
            resolver_minimum_confidence,
            audit_records: Mutex::new(Vec::new()),
        }
    }

    async fn execute(&self, query: &str) -> Result<ExecutionResult, ProxyError> {
        if let Some(result) = self.catalog_or_session_query(query) {
            return Ok(result);
        }

        let plan = parse_sql_for_profiles(query, &self.profiles)?;
        self.execute_plan(plan).await
    }

    async fn execute_bound(
        &self,
        query: &str,
        parameters: &[SqlValue],
    ) -> Result<ExecutionResult, ProxyError> {
        if let Some(result) = self.catalog_or_session_query(query) {
            return if parameters.is_empty() {
                Ok(result)
            } else {
                Err(ProxyError::new(
                    ErrorKind::InvalidInput,
                    "prepared statement parameters are not allowed for catalog or session queries",
                ))
            };
        }

        let plan = parse_sql_for_profiles(query, &self.profiles)?;
        let schema = self.schema_for_plan(&plan)?;
        let plan = bind_parameters(plan, parameters, schema)?;
        self.execute_plan(plan).await
    }

    async fn execute_plan(&self, plan: StatementPlan) -> Result<ExecutionResult, ProxyError> {
        match plan {
            StatementPlan::Select(plan) => self.execute_select(plan).await,
            plan @ (StatementPlan::Insert(_)
            | StatementPlan::Update(_)
            | StatementPlan::Delete(_)) => {
                let schema = self.schema_for_plan(&plan)?;
                let resolved = ambiguity::resolve_write_plan(
                    plan,
                    schema,
                    &self.resolver,
                    self.resolver_minimum_confidence,
                    &self.audit_records,
                )
                .await?;
                let outcome = match execute_write(&self.database, &resolved.plan).await {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        ambiguity::record_mongo_execution_failure(
                            schema,
                            resolved.audit_context.as_ref(),
                            &self.audit_records,
                        );
                        return Err(error);
                    }
                };
                ambiguity::record_execution(
                    schema,
                    resolved.audit_context.as_ref(),
                    outcome,
                    &self.audit_records,
                );
                Ok(ExecutionResult::Command(command_result(
                    &resolved.plan,
                    outcome,
                )))
            }
        }
    }

    fn describe(&self, query: &str) -> Result<Vec<WireField>, ProxyError> {
        if let Some(result) = self.catalog_or_session_query(query) {
            return Ok(result.fields());
        }
        match parse_sql_for_profiles(query, &self.profiles)? {
            StatementPlan::Select(plan) => {
                let catalog = self.catalog_for_collection(&plan.collection)?;
                Ok(Self::select_fields(&plan, catalog))
            }
            StatementPlan::Insert(_) | StatementPlan::Update(_) | StatementPlan::Delete(_) => {
                Ok(Vec::new())
            }
        }
    }

    async fn execute_select(&self, plan: SelectPlan) -> Result<ExecutionResult, ProxyError> {
        let catalog = self.catalog_for_collection(&plan.collection)?;
        let fields = Self::select_fields(&plan, catalog);
        let selected_paths = Self::selected_paths(&plan, catalog);
        let outcome = execute_select(&self.database, &plan).await?;
        let rows = outcome
            .documents
            .into_iter()
            .map(|document| {
                selected_paths
                    .iter()
                    .zip(&fields)
                    .map(|(path, field)| value_from_document(&document, path, field.sql_type))
                    .collect()
            })
            .collect();
        Ok(ExecutionResult::Query { fields, rows })
    }

    fn selected_paths(plan: &SelectPlan, catalog: &CollectionCatalog) -> Vec<Vec<String>> {
        match &plan.projection {
            Projection::All => catalog
                .columns
                .iter()
                .filter(|column| column.source_paths.len() == 1)
                .map(|column| column.source_paths[0].segments().to_vec())
                .collect(),
            Projection::Fields(fields) => fields
                .iter()
                .map(|field| field.path.segments().to_vec())
                .collect(),
        }
    }

    fn select_fields(plan: &SelectPlan, catalog: &CollectionCatalog) -> Vec<WireField> {
        match &plan.projection {
            Projection::All => catalog
                .columns
                .iter()
                .filter(|column| column.source_paths.len() == 1)
                .map(|column| WireField::new(column.column_name.clone(), column.sql_type))
                .collect(),
            Projection::Fields(fields) => fields
                .iter()
                .map(|field| {
                    let sql_type = catalog
                        .columns
                        .iter()
                        .find(|column| column.column_name == field.path.display_name())
                        .map_or(SqlType::Jsonb, |column| column.sql_type);
                    WireField::new(
                        field
                            .alias
                            .clone()
                            .unwrap_or_else(|| field.path.display_name()),
                        sql_type,
                    )
                })
                .collect(),
        }
    }

    fn schema_for_plan(&self, plan: &StatementPlan) -> Result<&SchemaProfile, ProxyError> {
        let collection = collection_from_plan(plan);
        self.profiles.get(collection).ok_or_else(|| {
            ProxyError::new(
                ErrorKind::InvalidInput,
                format!("collection '{collection}' is not in the configured collection allowlist"),
            )
        })
    }

    fn catalog_for_collection(&self, collection: &str) -> Result<&CollectionCatalog, ProxyError> {
        self.catalog.table(collection).ok_or_else(|| {
            ProxyError::new(
                ErrorKind::Dependency,
                format!("catalog projection is missing configured collection '{collection}'"),
            )
        })
    }

    fn catalog_or_session_query(&self, query: &str) -> Option<ExecutionResult> {
        let normalized = query.trim().trim_end_matches(';').to_ascii_lowercase();
        if normalized == "select 1" {
            return Some(query_result(
                vec![WireField::new("?column?", SqlType::BigInt)],
                vec![vec![WireValue::Integer(1)]],
            ));
        }
        if normalized.contains("current_database()") {
            return Some(query_result(
                vec![WireField::new("current_database", SqlType::Text)],
                vec![vec![WireValue::Text(self.database_name.clone())]],
            ));
        }
        if normalized.contains("current_schema()") {
            return Some(query_result(
                vec![WireField::new("current_schema", SqlType::Text)],
                vec![vec![WireValue::Text("public".to_owned())]],
            ));
        }
        if normalized.contains("version()") {
            return Some(query_result(
                vec![WireField::new("version", SqlType::Text)],
                vec![vec![WireValue::Text(
                    "PostgreSQL 16.0 (mongo-pg-proxy)".to_owned(),
                )]],
            ));
        }
        if normalized.starts_with("show server_version") {
            return Some(query_result(
                vec![WireField::new("server_version", SqlType::Text)],
                vec![vec![WireValue::Text("16.0".to_owned())]],
            ));
        }
        if normalized.contains("information_schema.tables") {
            return Some(self.information_schema_tables());
        }
        if normalized.contains("information_schema.columns") {
            return Some(self.information_schema_columns());
        }
        if normalized.contains("pg_catalog.pg_tables") {
            return Some(self.pg_tables());
        }
        if normalized.contains("pg_catalog.pg_class") {
            return Some(self.psql_relation_listing());
        }
        None
    }

    fn information_schema_tables(&self) -> ExecutionResult {
        query_result(
            vec![
                WireField::new("table_schema", SqlType::Text),
                WireField::new("table_name", SqlType::Text),
                WireField::new("table_type", SqlType::Text),
            ],
            self.catalog
                .tables()
                .map(|table| {
                    vec![
                        WireValue::Text(table.table.schema_name.clone()),
                        WireValue::Text(table.table.table_name.clone()),
                        WireValue::Text("BASE TABLE".to_owned()),
                    ]
                })
                .collect(),
        )
    }

    fn information_schema_columns(&self) -> ExecutionResult {
        let fields = vec![
            WireField::new("table_schema", SqlType::Text),
            WireField::new("table_name", SqlType::Text),
            WireField::new("column_name", SqlType::Text),
            WireField::new("ordinal_position", SqlType::BigInt),
            WireField::new("is_nullable", SqlType::Text),
            WireField::new("data_type", SqlType::Text),
            WireField::new("udt_name", SqlType::Text),
        ];
        let rows = self
            .catalog
            .columns()
            .map(|column| {
                vec![
                    WireValue::Text(column.table.schema_name.clone()),
                    WireValue::Text(column.table.table_name.clone()),
                    WireValue::Text(column.column_name.clone()),
                    WireValue::Integer(i64::from(column.ordinal_position)),
                    WireValue::Text(if column.is_nullable { "YES" } else { "NO" }.to_owned()),
                    WireValue::Text(column.sql_type.information_schema_name().to_owned()),
                    WireValue::Text(column.sql_type.udt_name().to_owned()),
                ]
            })
            .collect();
        query_result(fields, rows)
    }

    fn pg_tables(&self) -> ExecutionResult {
        query_result(
            vec![
                WireField::new("schemaname", SqlType::Text),
                WireField::new("tablename", SqlType::Text),
                WireField::new("tableowner", SqlType::Text),
                WireField::new("tablespace", SqlType::Text),
                WireField::new("hasindexes", SqlType::Boolean),
                WireField::new("hasrules", SqlType::Boolean),
                WireField::new("hastriggers", SqlType::Boolean),
                WireField::new("rowsecurity", SqlType::Boolean),
            ],
            self.catalog
                .tables()
                .map(|table| {
                    vec![
                        WireValue::Text(table.table.schema_name.clone()),
                        WireValue::Text(table.table.table_name.clone()),
                        WireValue::Text("mongo-pg-proxy".to_owned()),
                        WireValue::Null,
                        WireValue::Boolean(false),
                        WireValue::Boolean(false),
                        WireValue::Boolean(false),
                        WireValue::Boolean(false),
                    ]
                })
                .collect(),
        )
    }

    fn psql_relation_listing(&self) -> ExecutionResult {
        query_result(
            vec![
                WireField::new("Schema", SqlType::Text),
                WireField::new("Name", SqlType::Text),
                WireField::new("Type", SqlType::Text),
                WireField::new("Owner", SqlType::Text),
            ],
            self.catalog
                .tables()
                .map(|table| {
                    vec![
                        WireValue::Text(table.table.schema_name.clone()),
                        WireValue::Text(table.table.table_name.clone()),
                        WireValue::Text("table".to_owned()),
                        WireValue::Text("mongo-pg-proxy".to_owned()),
                    ]
                })
                .collect(),
        )
    }
}

fn infer_parameter_types(
    plan: &StatementPlan,
    schema: &SchemaProfile,
) -> Result<Vec<Type>, ProxyError> {
    let mut types = BTreeMap::<usize, Type>::new();
    match plan {
        StatementPlan::Select(plan) => {
            if let Some(predicate) = &plan.filter {
                collect_predicate_parameter_types(predicate, schema, &mut types)?;
            }
        }
        StatementPlan::Insert(plan) => {
            for row in &plan.rows {
                for (path, value) in plan.columns.iter().zip(row) {
                    collect_value_parameter_type(path, value, schema, &mut types)?;
                }
            }
        }
        StatementPlan::Update(plan) => {
            for assignment in &plan.assignments {
                collect_value_parameter_type(
                    &assignment.path,
                    &assignment.value,
                    schema,
                    &mut types,
                )?;
            }
            collect_predicate_parameter_types(&plan.filter, schema, &mut types)?;
        }
        StatementPlan::Delete(plan) => {
            collect_predicate_parameter_types(&plan.filter, schema, &mut types)?;
        }
    }

    let Some(maximum_index) = types.keys().next_back().copied() else {
        return Ok(Vec::new());
    };
    (1..=maximum_index)
        .map(|index| {
            types.get(&index).cloned().ok_or_else(|| {
                ProxyError::new(
                    ErrorKind::InvalidInput,
                    format!("prepared-statement placeholder ${index} has no type context"),
                )
            })
        })
        .collect()
}

fn prepare_statement(
    query: &str,
    requested_types: &[Type],
    profiles: &CollectionProfiles,
) -> Result<PreparedStatement, ProxyError> {
    if is_catalog_or_session_query(query) {
        return if requested_types.is_empty() {
            Ok(PreparedStatement {
                sql: query.to_owned(),
                parameter_types: Vec::new(),
            })
        } else {
            Err(ProxyError::new(
                ErrorKind::InvalidInput,
                "prepared statement parameters are not allowed for catalog or session queries",
            ))
        };
    }

    let plan = parse_sql_for_profiles(query, profiles)?;
    let schema = schema_for_plan_from_profiles(&plan, profiles)?;
    let inferred_types = infer_parameter_types(&plan, schema)?;
    let parameter_types = reconcile_parameter_types(requested_types, &inferred_types)?;
    Ok(PreparedStatement {
        sql: query.to_owned(),
        parameter_types,
    })
}

fn schema_for_plan_from_profiles<'a>(
    plan: &StatementPlan,
    profiles: &'a CollectionProfiles,
) -> Result<&'a SchemaProfile, ProxyError> {
    let collection = collection_from_plan(plan);
    profiles.get(collection).ok_or_else(|| {
        ProxyError::new(
            ErrorKind::InvalidInput,
            format!("collection '{collection}' is not in the configured collection allowlist"),
        )
    })
}

fn collect_predicate_parameter_types(
    predicate: &Predicate,
    schema: &SchemaProfile,
    types: &mut BTreeMap<usize, Type>,
) -> Result<(), ProxyError> {
    match predicate {
        Predicate::Compare { path, value, .. } => {
            collect_value_parameter_type(path, value, schema, types)
        }
        Predicate::In { path, values, .. } => values
            .iter()
            .try_for_each(|value| collect_value_parameter_type(path, value, schema, types)),
        Predicate::IsNull { .. } => Ok(()),
        Predicate::And(predicates) | Predicate::Or(predicates) => predicates
            .iter()
            .try_for_each(|predicate| collect_predicate_parameter_types(predicate, schema, types)),
    }
}

fn collect_value_parameter_type(
    path: &FieldPath,
    value: &SqlValue,
    schema: &SchemaProfile,
    types: &mut BTreeMap<usize, Type>,
) -> Result<(), ProxyError> {
    let SqlValue::Placeholder(placeholder) = value else {
        return Ok(());
    };
    let index = parse_placeholder_index(placeholder)?;
    let parameter_type = infer_field_parameter_type(path, schema)?;
    if let Some(existing) = types.insert(index, parameter_type.clone()) {
        if existing != parameter_type {
            return Err(ProxyError::new(
                ErrorKind::InvalidInput,
                format!(
                    "prepared-statement placeholder {placeholder} is used with incompatible field types"
                ),
            ));
        }
    }
    Ok(())
}

fn infer_field_parameter_type(
    path: &FieldPath,
    schema: &SchemaProfile,
) -> Result<Type, ProxyError> {
    let profile = schema.field(path).ok_or_else(|| {
        ProxyError::new(
            ErrorKind::InvalidInput,
            format!(
                "field '{}' is not present in the active schema profile",
                path.display_name()
            ),
        )
    })?;
    let non_null_types = profile
        .observed_types
        .iter()
        .copied()
        .filter(|observed_type| *observed_type != ObservedType::Null)
        .collect::<Vec<_>>();
    if profile.has_dotted_key_collision
        || profile.observed_shapes != [ObservedShape::Scalar].into()
        || non_null_types.len() != 1
    {
        return Err(ProxyError::new(
            ErrorKind::InvalidInput,
            format!(
                "cannot infer a safe prepared-statement type for ambiguous field '{}'",
                path.display_name()
            ),
        ));
    }
    match non_null_types[0] {
        ObservedType::Boolean => Ok(Type::BOOL),
        ObservedType::Integer => Ok(Type::INT8),
        ObservedType::FloatingPoint => Ok(Type::FLOAT8),
        ObservedType::String => Ok(Type::TEXT),
        ObservedType::Null
        | ObservedType::DateTime
        | ObservedType::ObjectId
        | ObservedType::Document
        | ObservedType::Array => Err(ProxyError::new(
            ErrorKind::InvalidInput,
            format!(
                "prepared-statement parameters are not supported for field '{}' with inferred BSON type '{}'",
                path.display_name(),
                observed_type_name(non_null_types[0])
            ),
        )),
    }
}

fn reconcile_parameter_types(
    requested_types: &[Type],
    inferred_types: &[Type],
) -> Result<Vec<Type>, ProxyError> {
    if requested_types.is_empty() {
        return Ok(inferred_types.to_vec());
    }
    if requested_types.len() != inferred_types.len() {
        return Err(ProxyError::new(
            ErrorKind::InvalidInput,
            format!(
                "prepared statement declares {} parameter type(s) but SQL requires {}",
                requested_types.len(),
                inferred_types.len()
            ),
        ));
    }
    requested_types
        .iter()
        .zip(inferred_types)
        .enumerate()
        .map(|(index, (requested, inferred))| {
            if *requested == Type::UNKNOWN {
                return Ok(inferred.clone());
            }
            if compatible_parameter_type(requested, inferred) {
                Ok(requested.clone())
            } else {
                Err(ProxyError::new(
                    ErrorKind::InvalidInput,
                    format!(
                        "prepared statement parameter ${} declares PostgreSQL type '{}' but schema requires '{}'",
                        index + 1,
                        requested.name(),
                        inferred.name()
                    ),
                ))
            }
        })
        .collect()
}

fn compatible_parameter_type(requested: &Type, inferred: &Type) -> bool {
    match *inferred {
        Type::BOOL => *requested == Type::BOOL,
        Type::INT8 => matches!(*requested, Type::INT2 | Type::INT4 | Type::INT8),
        Type::FLOAT8 => matches!(*requested, Type::FLOAT4 | Type::FLOAT8),
        Type::TEXT => matches!(*requested, Type::TEXT | Type::VARCHAR | Type::BPCHAR),
        _ => requested == inferred,
    }
}

fn parse_placeholder_index(placeholder: &str) -> Result<usize, ProxyError> {
    let Some(index) = placeholder.strip_prefix('$') else {
        return Err(ProxyError::new(
            ErrorKind::InvalidInput,
            format!(
                "unsupported prepared-statement placeholder '{placeholder}'; use PostgreSQL $n placeholders"
            ),
        ));
    };
    let index = index.parse::<usize>().map_err(|_| {
        ProxyError::new(
            ErrorKind::InvalidInput,
            format!(
                "invalid prepared-statement placeholder '{placeholder}'; expected $ followed by a positive integer"
            ),
        )
    })?;
    if index == 0 {
        return Err(ProxyError::new(
            ErrorKind::InvalidInput,
            "prepared-statement placeholder indexes start at $1",
        ));
    }
    Ok(index)
}

fn observed_type_name(observed_type: ObservedType) -> &'static str {
    match observed_type {
        ObservedType::Null => "null",
        ObservedType::Boolean => "boolean",
        ObservedType::Integer => "integer",
        ObservedType::FloatingPoint => "floating-point",
        ObservedType::String => "string",
        ObservedType::DateTime => "datetime",
        ObservedType::ObjectId => "object-id",
        ObservedType::Document => "document",
        ObservedType::Array => "array",
    }
}

#[derive(Debug, Clone)]
struct WireField {
    name: String,
    sql_type: SqlType,
}

impl WireField {
    fn new(name: impl Into<String>, sql_type: SqlType) -> Self {
        Self {
            name: name.into(),
            sql_type,
        }
    }

    fn field_info(&self, format: FieldFormat) -> FieldInfo {
        FieldInfo::new(
            self.name.clone(),
            None,
            None,
            postgres_type(self.sql_type),
            format,
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
enum WireValue {
    Null,
    Boolean(bool),
    Integer(i64),
    FloatingPoint(f64),
    Text(String),
}

#[derive(Debug)]
enum ExecutionResult {
    Query {
        fields: Vec<WireField>,
        rows: Vec<Vec<WireValue>>,
    },
    Command(CommandResult),
}

impl ExecutionResult {
    fn fields(&self) -> Vec<WireField> {
        match self {
            Self::Query { fields, .. } => fields.clone(),
            Self::Command(_) => Vec::new(),
        }
    }
}

#[derive(Debug)]
struct CommandResult {
    tag: &'static str,
    rows: usize,
}

fn query_result(fields: Vec<WireField>, rows: Vec<Vec<WireValue>>) -> ExecutionResult {
    ExecutionResult::Query { fields, rows }
}

fn command_result(plan: &StatementPlan, outcome: WriteOutcome) -> CommandResult {
    match plan {
        StatementPlan::Insert(_) => CommandResult {
            // PostgreSQL command tags include the legacy inserted OID before
            // the affected-row count. MongoDB-generated identifiers have no
            // compatible OID, so the protocol-standard value is zero.
            tag: "INSERT 0",
            rows: usize::try_from(outcome.inserted).unwrap_or(usize::MAX),
        },
        StatementPlan::Update(_) => CommandResult {
            tag: "UPDATE",
            rows: usize::try_from(outcome.matched).unwrap_or(usize::MAX),
        },
        StatementPlan::Delete(_) => CommandResult {
            tag: "DELETE",
            rows: usize::try_from(outcome.deleted).unwrap_or(usize::MAX),
        },
        StatementPlan::Select(_) => unreachable!("SELECT is not a command result"),
    }
}

fn collection_from_plan(plan: &StatementPlan) -> &str {
    match plan {
        StatementPlan::Select(plan) => &plan.collection,
        StatementPlan::Insert(plan) => &plan.collection,
        StatementPlan::Update(plan) => &plan.collection,
        StatementPlan::Delete(plan) => &plan.collection,
    }
}

fn postgres_type(sql_type: SqlType) -> Type {
    match sql_type {
        SqlType::Boolean => Type::BOOL,
        SqlType::BigInt => Type::INT8,
        SqlType::DoublePrecision => Type::FLOAT8,
        SqlType::Text => Type::TEXT,
        SqlType::TimestampWithTimeZone => Type::TIMESTAMPTZ,
        SqlType::Jsonb => Type::JSONB,
    }
}

fn value_from_document(document: &Document, path: &[String], sql_type: SqlType) -> WireValue {
    let value = value_at_path(document, path);
    let Some(value) = value else {
        return WireValue::Null;
    };
    bson_to_wire_value(value, sql_type)
}

fn value_at_path<'a>(document: &'a Document, path: &[String]) -> Option<&'a Bson> {
    let (last, parents) = path.split_last()?;
    let nested = parents.iter().try_fold(document, |current, segment| {
        current.get_document(segment).ok()
    })?;
    nested.get(last)
}

fn bson_to_wire_value(value: &Bson, sql_type: SqlType) -> WireValue {
    match value {
        Bson::Null | Bson::Undefined => WireValue::Null,
        Bson::Boolean(value) => WireValue::Boolean(*value),
        Bson::Int32(value) => WireValue::Integer(i64::from(*value)),
        Bson::Int64(value) => WireValue::Integer(*value),
        Bson::Double(value) => WireValue::FloatingPoint(*value),
        Bson::String(value) => WireValue::Text(value.clone()),
        Bson::ObjectId(value) => WireValue::Text(value.to_hex()),
        Bson::DateTime(value) => WireValue::Text(value.to_string()),
        value if sql_type == SqlType::Jsonb => {
            WireValue::Text(value.clone().into_relaxed_extjson().to_string())
        }
        value => WireValue::Text(value.to_string()),
    }
}

fn response_from_result<'a>(result: ExecutionResult, format: &Format) -> Response<'a> {
    match result {
        ExecutionResult::Command(command) => {
            Response::Execution(Tag::new(command.tag).with_rows(command.rows))
        }
        ExecutionResult::Query { fields, rows } => {
            let schema: Arc<Vec<FieldInfo>> = Arc::new(
                fields
                    .iter()
                    .enumerate()
                    .map(|(index, field)| field.field_info(format.format_for(index)))
                    .collect(),
            );
            let schema_for_rows = schema.clone();
            let row_stream = stream::iter(rows).map(move |row| {
                let mut encoder = DataRowEncoder::new(schema_for_rows.clone());
                for value in row {
                    match value {
                        WireValue::Null => encoder.encode_field(&None::<i64>)?,
                        WireValue::Boolean(value) => encoder.encode_field(&value)?,
                        WireValue::Integer(value) => encoder.encode_field(&value)?,
                        WireValue::FloatingPoint(value) => encoder.encode_field(&value)?,
                        WireValue::Text(value) => encoder.encode_field(&value)?,
                    }
                }
                encoder.finish()
            });
            Response::Query(QueryResponse::new(schema, row_stream))
        }
    }
}

fn proxy_error_to_pgwire(error: ProxyError) -> PgWireError {
    PgWireError::UserError(Box::new(ErrorInfo::new(
        "ERROR".to_owned(),
        error.kind.sql_state().to_owned(),
        error.message,
    )))
}

struct ProxyHandlerFactory {
    backend: Arc<ProxyBackend>,
    auth: ProxyAuthConfig,
}

impl PgWireHandlerFactory for ProxyHandlerFactory {
    type StartupHandler = ProxyStartupHandler;
    type SimpleQueryHandler = ProxyBackend;
    type ExtendedQueryHandler = ProxyBackend;
    type CopyHandler = NoopCopyHandler;

    fn simple_query_handler(&self) -> Arc<Self::SimpleQueryHandler> {
        self.backend.clone()
    }

    fn extended_query_handler(&self) -> Arc<Self::ExtendedQueryHandler> {
        self.backend.clone()
    }

    fn startup_handler(&self) -> Arc<Self::StartupHandler> {
        Arc::new(ProxyStartupHandler::new(self.auth.clone()))
    }

    fn copy_handler(&self) -> Arc<Self::CopyHandler> {
        Arc::new(NoopCopyHandler)
    }
}

#[async_trait]
impl SimpleQueryHandler for ProxyBackend {
    async fn do_query<'a, C>(
        &self,
        _client: &mut C,
        query: &'a str,
    ) -> PgWireResult<Vec<Response<'a>>>
    where
        C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        self.execute(query)
            .await
            .map_err(proxy_error_to_pgwire)
            .map(|result| response_from_result(result, &Format::UnifiedText))
            .map(|response| vec![response])
    }
}

#[async_trait]
impl ExtendedQueryHandler for ProxyBackend {
    type Statement = PreparedStatement;
    type QueryParser = ProxyQueryParser;

    fn query_parser(&self) -> Arc<Self::QueryParser> {
        Arc::new(ProxyQueryParser {
            profiles: self.profiles.clone(),
        })
    }

    async fn do_query<'a, C>(
        &self,
        _client: &mut C,
        portal: &'a Portal<Self::Statement>,
        _max_rows: usize,
    ) -> PgWireResult<Response<'a>>
    where
        C: ClientInfo + Unpin + Send + Sync,
    {
        let parameters = portal_parameters(portal).map_err(proxy_error_to_pgwire)?;
        self.execute_bound(&portal.statement.statement.sql, &parameters)
            .await
            .map_err(proxy_error_to_pgwire)
            .map(|result| response_from_result(result, &portal.result_column_format))
    }

    async fn do_describe_statement<C>(
        &self,
        _client: &mut C,
        statement: &StoredStatement<Self::Statement>,
    ) -> PgWireResult<DescribeStatementResponse>
    where
        C: ClientInfo + Unpin + Send + Sync,
    {
        self.describe(&statement.statement.sql)
            .map_err(proxy_error_to_pgwire)
            .map(|fields| {
                DescribeStatementResponse::new(
                    statement.statement.parameter_types.clone(),
                    fields
                        .into_iter()
                        .map(|field| field.field_info(FieldFormat::Text))
                        .collect(),
                )
            })
    }

    async fn do_describe_portal<C>(
        &self,
        _client: &mut C,
        portal: &Portal<Self::Statement>,
    ) -> PgWireResult<DescribePortalResponse>
    where
        C: ClientInfo + Unpin + Send + Sync,
    {
        self.describe(&portal.statement.statement.sql)
            .map_err(proxy_error_to_pgwire)
            .map(|fields| {
                DescribePortalResponse::new(
                    fields
                        .into_iter()
                        .enumerate()
                        .map(|(index, field)| {
                            field.field_info(portal.result_column_format.format_for(index))
                        })
                        .collect(),
                )
            })
    }
}

fn portal_parameters(portal: &Portal<PreparedStatement>) -> Result<Vec<SqlValue>, ProxyError> {
    let parameter_types = &portal.statement.statement.parameter_types;
    if portal.parameter_len() != parameter_types.len() {
        return Err(ProxyError::new(
            ErrorKind::InvalidInput,
            format!(
                "prepared statement supplies {} values but declares {} parameter types; explicit supported PostgreSQL OIDs are required",
                portal.parameter_len(),
                parameter_types.len()
            ),
        ));
    }

    portal
        .parameters
        .iter()
        .zip(parameter_types)
        .enumerate()
        .map(|(index, (value, parameter_type))| {
            let format = parameters::parameter_format(&portal.parameter_format, index)?;
            parameters::decode_parameter(value.as_deref(), parameter_type, format)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, time::Duration};

    use mongo_pg_ambiguity_policy::audit::{AuditFailure, AuditOutcome};
    use mongo_pg_common::ErrorKind;
    use mongo_pg_resolver_client::{ResolverClient, ResolverClientConfig};
    use mongo_pg_schema_discovery::{SampleDocument, SampleValue, SchemaProfile};

    use super::{
        ExecutionResult, ProxyBackend, WireValue, bson_to_wire_value,
        parse_configured_collection_names,
    };

    fn document(fields: impl IntoIterator<Item = (&'static str, SampleValue)>) -> SampleDocument {
        fields
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value))
            .collect::<BTreeMap<_, _>>()
    }

    fn resolver_client() -> ResolverClient {
        let config = ResolverClientConfig::new(
            "http://127.0.0.1:8000/v1/resolve",
            Duration::from_millis(10),
        );
        ResolverClient::new(&config).expect("test resolver endpoint is valid")
    }

    #[tokio::test]
    async fn catalog_query_reports_schema_backed_column_metadata() {
        let profile = mongo_pg_schema_discovery::SchemaProfile::infer(&[document([
            ("enabled", SampleValue::Boolean(true)),
            ("name", SampleValue::String("Ada".to_owned())),
        ])]);
        let backend = ProxyBackend::new(
            mongodb::Client::with_uri_str("mongodb://localhost:27017")
                .await
                .expect("valid URI")
                .database("demo"),
            "demo".to_owned(),
            BTreeMap::from([("customers".to_owned(), profile)]),
            resolver_client(),
            0.8,
        );
        let result = backend
            .catalog_or_session_query("SELECT * FROM information_schema.columns")
            .expect("catalog query should be handled");
        let fields = result.fields();
        assert_eq!(fields.len(), 7);
    }

    #[test]
    fn jsonb_values_are_encoded_as_extended_json_text() {
        assert_eq!(
            bson_to_wire_value(
                &mongodb::bson::Bson::Document(mongodb::bson::doc! { "city": "Harare" }),
                mongo_pg_catalog::SqlType::Jsonb,
            ),
            WireValue::Text(r#"{"city":"Harare"}"#.to_owned())
        );
    }

    #[test]
    fn collection_list_overrides_the_legacy_single_collection_setting() {
        let collections =
            parse_configured_collection_names(Some("orders, customers"), Some("legacy_customers"))
                .expect("collection list should be valid");
        assert_eq!(collections, vec!["customers", "orders"]);

        let fallback = parse_configured_collection_names(Some(""), Some("customers"))
            .expect("empty Compose list should use the legacy fallback");
        assert_eq!(fallback, vec!["customers"]);
    }

    #[tokio::test]
    async fn blocks_ambiguous_writes_before_calling_mongodb() {
        let profile = SchemaProfile::infer(&[
            document([
                ("name", SampleValue::String("Amina".to_owned())),
                ("status", SampleValue::Integer(1)),
            ]),
            document([
                ("name", SampleValue::String("Tendai".to_owned())),
                ("status", SampleValue::String("active".to_owned())),
            ]),
        ]);
        let backend = ProxyBackend::new(
            mongodb::Client::with_uri_str("mongodb://localhost:27017")
                .await
                .expect("valid URI")
                .database("demo"),
            "demo".to_owned(),
            BTreeMap::from([("customers".to_owned(), profile)]),
            resolver_client(),
            0.8,
        );

        let error = backend
            .execute("UPDATE customers SET status = true WHERE name = 'Amina'")
            .await
            .expect_err("ambiguous write must not reach MongoDB");
        assert_eq!(error.kind, ErrorKind::AmbiguousWrite);
        let records = backend
            .audit_records
            .lock()
            .expect("audit records are available");
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].outcome,
            AuditOutcome::Blocked(AuditFailure::NoSafeResolution)
        );
    }

    #[tokio::test]
    async fn exposes_and_routes_each_allowlisted_collection_in_isolation() {
        let customers = SchemaProfile::infer(&[document([(
            "name",
            SampleValue::String("Amina".to_owned()),
        )])]);
        let orders = SchemaProfile::infer(&[document([("order_total", SampleValue::Integer(42))])]);
        let backend = ProxyBackend::new(
            mongodb::Client::with_uri_str("mongodb://localhost:27017")
                .await
                .expect("valid URI")
                .database("demo"),
            "demo".to_owned(),
            BTreeMap::from([
                ("customers".to_owned(), customers),
                ("orders".to_owned(), orders),
            ]),
            resolver_client(),
            0.8,
        );

        let tables = backend
            .catalog_or_session_query("SELECT * FROM information_schema.tables")
            .expect("catalog query should be handled");
        let ExecutionResult::Query { rows, .. } = tables else {
            panic!("catalog query must return rows");
        };
        assert_eq!(rows.len(), 2);
        assert!(
            rows.iter()
                .any(|row| { row.get(1) == Some(&WireValue::Text("customers".to_owned())) })
        );
        assert!(
            rows.iter()
                .any(|row| row.get(1) == Some(&WireValue::Text("orders".to_owned())))
        );

        let fields = backend
            .describe("SELECT order_total FROM orders")
            .expect("orders profile should resolve its own field");
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].name, "order_total");
        assert_eq!(fields[0].sql_type, mongo_pg_catalog::SqlType::BigInt);

        let leaked_schema = backend
            .describe("SELECT name FROM orders")
            .expect_err("customers fields must not resolve against orders");
        assert_eq!(leaked_schema.kind, ErrorKind::InvalidInput);

        let leaked_write = backend
            .execute("UPDATE orders SET name = 'wrong table' WHERE order_total = 42")
            .await
            .expect_err("write assignments must use the selected collection profile");
        assert_eq!(leaked_write.kind, ErrorKind::InvalidInput);
    }
}
