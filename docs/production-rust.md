# Production Rust Patterns

> Looking for testing guidance? See [`testing-handbook.md`](./testing-handbook.md)
> for integration, property, and concurrency testing practices.

Patterns and anti-patterns for production-grade Rust code, please extend this to keep our robot friends on the straight and narrow.

---

## Error Handling

### ✅ Pattern: Never Unwrap in Production

**Bad:**

```rust
let value: Uuid = row.try_get("id").unwrap();  // ❌ PANIC on schema change!
let amount = req.amount.parse::<i128>().unwrap();  // ❌ PANIC on bad input!
```

**Good:**

```rust
let value: Uuid = row.try_get("id")
    .map_err(|e| MyError::RowExtraction(format!("id: {}", e)))?;

let amount = req.amount.parse::<i128>()
    .map_err(|_| MyError::InvalidAmount)?;
```

**When unwrap is acceptable:**

- Test code only
- After explicit validation that guarantees success
- With detailed comment explaining why it's safe

### ❌ Anti-Pattern: Silently Discarding Errors with `let _ =`

**Bad:**

```rust
// ❌ Silently ignores errors - failures go unnoticed!
let _ = self.user_identity_store.create_profile(...).await;
let _ = external_function().await;
let _ = fallible_operation()?;  // Still ignores the error!

// ❌ Even worse: discarding Result without checking
let _ = may_fail().await;
```

**Good:**

```rust
// ✅ Propagate errors properly
self.user_identity_store.create_profile(...).await?;

// ✅ Handle errors explicitly
match external_function().await {
    Ok(result) => {
        tracing::debug!(result = ?result, "Operation succeeded");
    }
    Err(e) => {
        tracing::warn!(error = ?e, "Operation failed, continuing");
        // Only discard if you've explicitly handled the error case
    }
}

// ✅ Use error handling for side effects
if let Err(e) = cleanup_operation().await {
    tracing::warn!(error = ?e, "Cleanup failed, continuing");
}
```

**When `let _ =` is acceptable:**

- Test code only (with explicit `.unwrap()` or `.expect()`)
- Operations that are intentionally fire-and-forget with proper logging
- Operations where failure is explicitly handled via logging/tracing
- With detailed comment explaining why discarding is safe

**Critical Rule:**

Never use `let _ = fallible_operation().await;` without explicit error handling. If you need to ignore an error, you must:

1. Log/trace the error with context
2. Document why ignoring is safe
3. Consider if the operation should actually fail the calling function

**Why This Matters:**

- Silent failures hide bugs in production
- Makes debugging nearly impossible
- Breaks error propagation chains
- Can lead to inconsistent state (partial operations succeed/fail silently)

### ❌ Anti-Pattern: Fallbacks for Backwards Compatibility

**Bad:**

```rust
// ❌ Fallback to old behavior "just in case" - creates maintenance burden
let agent_id = metadata.get("agent_id")
    .map(|s| AgentId::from_uuid(UuidId::parse_str(s).map_err(|_| "invalid uuid")?))
    .or_else(|| {
        // Fallback: look up by agent_type (legacy support)
        let agent_type = metadata.get("agent_type")?;
        find_agent_by_type(doc, agent_type)
    })
    .unwrap_or_else(|| {
        // Fallback: create temporary agent node
        create_temp_agent(agent_type)
    });

// ❌ Fallback logic that supports old data formats
let value = parse_new_format(input)
    .or_else(|_| parse_old_format(input))  // Backwards compatibility fallback
    .unwrap_or_else(|_| default_value());  // Another fallback
```

**Good:**

```rust
// ✅ Require correct data - fail fast if missing
let agent_id = metadata.get("agent_id")
    .ok_or(MyError::MissingAgentId)?
    .parse::<AgentId>()
    .map_err(|e| MyError::InvalidAgentId(e))?;

// ✅ For operations that might actually fail (network, filesystem), use retries
async fn fetch_with_retry(url: &str) -> Result<Response> {
    for attempt in 1..=3 {
        match http_client.get(url).send().await {
            Ok(resp) => return Ok(resp),
            Err(e) if attempt < 3 => {
                tokio::time::sleep(Duration::from_millis(100 * attempt)).await;
                continue;
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}
```

**When Fallbacks Are Acceptable:**

- ✅ **Network operations** - Retries for transient failures (timeouts, connection errors)
- ✅ **File system operations** - Fallback paths for missing config files (with explicit logging)
- ✅ **External service calls** - Retry logic for rate limits or temporary unavailability
- ✅ **Resource allocation** - Fallback to alternative resources when primary is unavailable
- ✅ **When explicitly requested** - If backwards compatibility is an explicit requirement, document it clearly

**When Fallbacks Are NOT Acceptable:**

- ❌ **Backwards compatibility** - Don't support old data formats "just in case"
- ❌ **Missing required data** - Don't fallback to defaults for required fields
- ❌ **Type mismatches** - Don't fallback to alternative types when the correct type is missing
- ❌ **Domain invariants** - Don't fallback when domain rules require specific data
- ❌ **Silent degradation** - Don't fallback to less-capable behavior without explicit user request

**Critical Rule:**

**Fallbacks are for handling operations that might actually fail (network, filesystem, external services), not for representing backwards compatibility we may not need.**

If you are instructed to support backwards compatibility, I will be explicit about it. Otherwise, require the correct data and fail fast with clear errors.

**Why This Matters:**

- **Prevents technical debt** - Old code paths accumulate bugs and maintenance burden
- **Enforces data correctness** - Failures surface data quality issues early
- **Clearer error messages** - Missing required data produces explicit errors, not silent fallbacks
- **Simpler code** - No need to maintain multiple code paths
- **Type safety** - Compiler enforces correct data structures, not runtime fallbacks

### ❌ Anti-Pattern: Stringifying Errors and Discarding Context

**Bad:**

```rust
// ❌ Loses all error context and debugging information
let result = external_crate_function()
    .map_err(|e| MyError::ExternalError(format!("External call failed: {}", e)))?;

// ❌ Converts structured error to string, losing the original error type
let config = load_config()
    .map_err(|e| MyError::ConfigError(format!("Failed to load config: {}", e)))?;
```

**Good:**

```rust
// ✅ Error names encode the OPERATION that caused them, not just the crate
let result = secret_vault_function()
    .map_err(|e| MyError::VaultRetrieval(e))?;

let signing_result = alloy_signing_function()
    .map_err(|e| MyError::KeySigning(e))?;

let config = load_config()
    .map_err(|e| MyError::ConfigLoading(e))?;

// ✅ Use #[from] attribute for automatic conversion
#[derive(Debug, Error)]
pub enum MyError {
    #[error("Failed to retrieve key from vault: {0}")]
    VaultRetrieval(#[from] secret_vault::errors::SecretVaultError),

    #[error("Failed to sign transaction: {0}")]
    KeySigning(#[from] alloy::signers::k256::ecdsa::Error),

    #[error("Failed to load configuration: {0}")]
    ConfigLoading(#[from] std::io::Error),

    #[error("Failed to serialize data: {0}")]
    DataSerialization(#[from] serde_json::Error),

    #[error("Failed to encode key: {0}")]
    KeyEncoding(#[from] base64::DecodeError),
}
```

**Error Naming Convention:**

- ✅ **Operation-based names**: `VaultRetrieval`, `KeySigning`, `ConfigLoading`
- ✅ **Self-documenting**: Error name tells you what operation failed
- ✅ **Specific context**: Each error variant maps to a specific operation
- ❌ **Avoid generic names**: `External`, `Io`, `Serialization` (too vague)
- ❌ **Avoid crate names**: `SecretVault`, `Alloy`, `Serde` (doesn't tell you what failed)

**When string conversion is acceptable:**

- Only for errors that originate entirely within your own code
- When the error message contains all necessary context
- For domain-specific validation errors that don't wrap external errors

**Benefits of specific error variants:**

- **Preserves error chains** for better debugging
- **Maintains original error types** for programmatic handling
- **Enables proper error propagation** through the call stack
- **Allows error source tracing** in production logs
- **Enables pattern matching** on specific error types
- **Provides type safety** for error handling logic
- **Makes error handling explicit** and self-documenting

### ✅ Pattern: RFC 7807 Problem Details for HTTP Errors

**All HTTP error responses must use RFC 7807 Problem Details format via `http-api-problem` crate.**

**Bad (Ad-hoc Error Responses):**

```rust
// ❌ Inconsistent error formats across endpoints
pub async fn create_api_key(...) -> Result<Json<Response>, (StatusCode, String)> {
    if profile_not_found {
        return Err((StatusCode::NOT_FOUND, "Profile not found".to_string()));
    }
    if invalid_input {
        return Err((StatusCode::BAD_REQUEST, "Invalid input".to_string()));
    }
    // ...
}

// ❌ Manual JSON construction
pub async fn process_payment(...) -> Result<Json<Response>, (StatusCode, Json<serde_json::Value>)> {
    if insufficient_balance {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error": "Insufficient balance"})),
        ));
    }
    // ...
}

// ❌ Different error formats in same API
// Some endpoints return: {"error": "message"}
// Others return: {"message": "error"}
// Others return: (StatusCode, String)
```

**Good (RFC 7807 Problem Details):**

```rust
use http_api_problem::{HttpApiProblem, StatusCode as ProblemStatusCode};

/// Problem type constants for module-specific errors (RFC 7807).
///
/// Uses relative URIs to support dynamic domains.
mod problem_types {
    pub const INSUFFICIENT_BALANCE: &str = "/problems/insufficient-balance";
    pub const PROFILE_NOT_FOUND: &str = "/problems/profile-not-found";
    pub const INVALID_PROFILE_ID: &str = "/problems/invalid-profile-id";
    pub const INTERNAL_ERROR: &str = "/problems/internal-error";
}

/// HTTP result type alias for cleaner signatures.
type HttpResult<T> = Result<Json<T>, HttpApiProblem>;

// ✅ Convert domain errors to HttpApiProblem
impl From<AccountingError> for HttpApiProblem {
    fn from(e: AccountingError) -> Self {
        use problem_types::*;

        match e {
            AccountingError::InsufficientBalance { account_id, required, available } => {
                HttpApiProblem::new(ProblemStatusCode::UNPROCESSABLE_ENTITY)
                    .title("Insufficient balance")
                    .type_url(INSUFFICIENT_BALANCE)
                    .detail(&format!(
                        "Account {} has insufficient balance: required {}, available {}",
                        account_id.0, required, available
                    ))
            }
            AccountingError::Sqlx(_) | AccountingError::Db(_) => {
                HttpApiProblem::new(ProblemStatusCode::INTERNAL_SERVER_ERROR)
                    .title("Storage error")
                    .type_url(INTERNAL_ERROR)
                    .detail("Internal database error")
            }
            // ... other variants
        }
    }
}

// ✅ Handlers return HttpApiProblem directly
pub async fn process_payment(
    State(store): State<AccountingStore>,
    Json(req): Json<ProcessPaymentRequest>,
) -> HttpResult<PaymentResponse> {
    let result = store.process_payment(&req.from, &req.to, amount, metadata).await;

    // ✅ Automatic conversion via From trait
    result.map_err(HttpApiProblem::from)?;

    // ...
}

// ✅ Direct HttpApiProblem construction for validation errors
pub async fn create_api_key(
    Path(profile_id_str): Path<String>,
    // ...
) -> HttpResult<ApiKeyCreationResponse> {
    use problem_types::*;

    let profile_id = Uuid::parse_str(&profile_id_str)
        .map_err(|_| {
            HttpApiProblem::new(ProblemStatusCode::BAD_REQUEST)
                .title("Invalid profile ID")
                .type_url(INVALID_PROFILE_ID)
                .detail("Profile ID must be a valid UUID")
        })?;

    // ...
}
```

**Problem Type Guidelines:**

1. **Use relative URIs** for problem types (e.g., `/problems/insufficient-balance`)

   - Supports dynamic domains and different environments
   - Clients can resolve relative to request origin

2. **Organize by module** - Each module/library exports its own `problem_types` submodule:

   ```rust
   // lib/credit-accounting/src/http.rs
   pub mod problem_types {
       pub const INSUFFICIENT_BALANCE: &str = "/problems/insufficient-balance";
       pub const SYSTEM_ACCOUNT_CREDIT: &str = "/problems/system-account-credit";
   }

   // apps/server/src/auth/errors.rs
   pub mod problem_types {
       pub const INVALID_TOKEN: &str = "/problems/invalid-token";
       pub const MISSING_CREDENTIALS: &str = "/problems/missing-credentials";
   }
   ```

3. **Use descriptive, kebab-case names** that match the error semantics:

   - ✅ `/problems/insufficient-balance` (clear, specific)
   - ✅ `/problems/invalid-profile-id` (describes the validation failure)
   - ❌ `/problems/error` (too generic)
   - ❌ `/problems/bad-request` (HTTP status, not problem type)

4. **Map domain errors to appropriate HTTP status codes:**
   - `400 Bad Request` - Client validation errors (invalid UUID, missing required field)
   - `401 Unauthorized` - Authentication failures (invalid token, missing credentials)
   - `404 Not Found` - Resource doesn't exist (profile not found, API key not found)
   - `422 Unprocessable Entity` - Business logic violations (insufficient balance)
   - `500 Internal Server Error` - Unexpected server errors (database failures)

**Benefits:**

- **Consistent error format** across all endpoints
- **Machine-readable** - clients can programmatically handle specific error types
- **Self-documenting** - problem types encode error semantics
- **RFC 7807 compliant** - standard format for HTTP APIs
- **Type-safe** - `HttpApiProblem` implements `IntoResponse` for Axum
- **Automatic conversion** - `From<DomainError>` trait enables ergonomic error handling

**When to Use Direct Construction vs From Trait:**

- ✅ **Use `From<DomainError>`** for domain errors that occur in business logic
- ✅ **Use direct construction** for HTTP-level validation (invalid UUID format, missing headers)
- ✅ **Use helper functions** for common patterns (see `server::errors::problem()`)

### ❌ Anti-Pattern: Inconsistent HTTP Error Formats

**Bad:**

```rust
// ❌ Different formats in same API
pub async fn endpoint1(...) -> Result<Json<T>, (StatusCode, String)> {
    Err((StatusCode::NOT_FOUND, "Not found".to_string()))
}

pub async fn endpoint2(...) -> Result<Json<T>, (StatusCode, Json<serde_json::Value>)> {
    Err((StatusCode::BAD_REQUEST, Json(json!({"error": "Invalid"}))))
}

pub async fn endpoint3(...) -> Result<Json<T>, String> {
    Err("Something went wrong".to_string())
}

// ❌ Manual JSON construction
let error_response = Json(json!({
    "error": "Insufficient balance",
    "code": 422,
    "message": "Account has insufficient balance"
}));
```

**Good:**

```rust
// ✅ Consistent HttpApiProblem format everywhere
pub async fn endpoint1(...) -> HttpResult<T> {
    Err(HttpApiProblem::new(ProblemStatusCode::NOT_FOUND)
        .title("Not found")
        .type_url("/problems/not-found")
        .detail("Resource not found"))
}

pub async fn endpoint2(...) -> HttpResult<T> {
    Err(HttpApiProblem::new(ProblemStatusCode::BAD_REQUEST)
        .title("Invalid request")
        .type_url("/problems/invalid-request")
        .detail("Request validation failed"))
}

// ✅ Automatic conversion from domain errors
pub async fn endpoint3(...) -> HttpResult<T> {
    let result = domain_operation().await;
    result.map_err(HttpApiProblem::from)?;  // ✅ Automatic conversion
}
```

**Why This Matters:**

- **Client confusion** - Different error formats require different parsing logic
- **Inconsistent debugging** - Hard to trace errors across endpoints
- **Non-standard** - Doesn't follow RFC 7807 or common API conventions
- **Maintenance burden** - Each endpoint handles errors differently

---

## Type Design

### ✅ Pattern: Use Strong Types at Boundaries

**Boundaries are serialization points where data crosses system layers:**

- **HTTP requests/responses** (JSON/form data)
- **Database rows** (sqlx type conversions)
- **Message queues** (protocol buffers, JSON)
- **External APIs** (webhooks, RPC calls)

At these boundaries, use newtypes with serde/sqlx derives to enforce validation once.

**Bad:**

```rust
// HTTP boundary - forces manual parsing everywhere
pub struct PaymentRequest {
    pub amount: String,  // ❌ Caller must parse!
}

async fn process(req: PaymentRequest) {
    let amount = req.amount.parse::<i128>()?;  // Repeated in every handler
    // ...
}

// Database boundary - row extraction is error-prone
let amount_str: String = row.try_get("amount")?;
let amount = amount_str.parse::<i128>()?;  // Repeated for every query
```

**Good:**

```rust
// HTTP boundary - serde validates at deserialization
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
pub struct Amount(#[serde(with = "amount_as_string")] i128);

pub struct PaymentRequest {
    pub amount: Amount,  // ✅ Parsed by serde automatically
}

async fn process(req: PaymentRequest) {
    let amount = req.amount.as_i128();  // ✅ Zero validation needed
    // ...
}

// Database boundary - sqlx validates at row extraction
impl sqlx::Type<Postgres> for Amount {
    fn type_info() -> PgTypeInfo {
        <BigDecimal as sqlx::Type<Postgres>>::type_info()
    }
}

let amount: Amount = row.try_get("amount")?;  // ✅ Automatic conversion
```

**Benefits:**

- Single source of validation (at the boundary)
- Impossible to forget parsing
- Type safety prevents mistakes
- Serde/sqlx handle serialization
- Domain logic works with rich types, not primitives

**Example: Timestamp with RFC3339 Serialization**

Instead of manually serializing datetime fields in every struct:

```rust
// ❌ Bad: Manual serialization everywhere
#[derive(Serialize)]
pub struct Response {
    #[serde(serialize_with = "serialize_datetime_as_rfc3339")]
    pub created_at: DateTime<Utc>,
    #[serde(serialize_with = "serialize_datetime_as_rfc3339")]  // Repeated!
    pub updated_at: DateTime<Utc>,
}

fn serialize_datetime_as_rfc3339<S>(value: &DateTime<Utc>, s: S) -> Result<S::Ok, S::Error>
where S: Serializer { /* ... */ }
```

Use a newtype that handles serialization once:

```rust
// ✅ Good: Newtype with built-in serialization
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Timestamp(pub DateTime<Utc>);

impl Serialize for Timestamp {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where S: Serializer {
        serializer.serialize_str(&self.0.to_rfc3339())
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where D: Deserializer<'de> {
        let s = String::deserialize(deserializer)?;
        DateTime::parse_from_rfc3339(&s)
            .map(|dt| Self(dt.with_timezone(&Utc)))
            .map_err(serde::de::Error::custom)
    }
}

// Now use it everywhere - no manual serialization needed!
#[derive(Serialize, Deserialize)]
pub struct Response {
    pub created_at: Timestamp,  // ✅ Automatic RFC3339
    pub updated_at: Timestamp,  // ✅ Automatic RFC3339
}
```

---

### ✅ Pattern: Model Domain Structure with Discriminated Unions (Invalid States Unrepresentable)

**The Problem:**

Optional fields encode structure implicitly and allow invalid combinations to compile.
That means higher-level dependencies (ordering, scope, and required relationships) are
not enforced by the type system.

Examples of *structural* invalid states that should be impossible:

- A task-scoped event without a `TaskId`
- A task without an `AgentType`
- A "completed" call without `duration_ms` / `success` / `usage`
- A message that implies a task relation but carries no task scope

If the structure of the domain is encoded as `Option` fields, the compiler cannot
prevent impossible graph shapes.

**Bad (Option Combinations):**

```rust
// ❌ Optionality hides domain structure and allows invalid combos.
pub struct LlmCall {
    pub task_id: Option<TaskId>,          // required for task-scoped calls
    pub duration_ms: Option<u64>,         // set only on completion
    pub success: Option<bool>,            // set only on completion
    pub agent_type: Option<String>,       // required by design
}
```

**Good (Discriminated Unions / Structure-Enforced Types):**

```rust
// ✅ Domain states are explicit and invalid combos are unrepresentable.
pub enum LlmCall {
    Started(LlmCallStarted),
    Completed(LlmCallCompleted),
}

pub enum CallScope {
    Message(MessageId),
    Task(TaskId),
}

pub struct LlmCallStarted {
    pub scope: CallScope,
    pub client: String,
    pub model: String,
    pub prompt: Value,
}

pub struct LlmCallCompleted {
    pub scope: CallScope,
    pub client: String,
    pub model: String,
    pub prompt: Value,
    pub duration_ms: u64,
    pub success: bool,
    pub usage: LlmUsage,
}
```

**Scope-Explicit Events (Structure-Enforced):**

```rust
// ✅ Scope is explicit, not an Option.
pub enum ProvEvent {
    Task(TaskEvent),
    Global(GlobalEvent),
}

pub struct TaskEvent {
    pub task_id: TaskId,
    pub agent_type: AgentType,
    pub data: TaskEventData,
}
```

**Benefits:**

- **Invalid states are unrepresentable** (compile-time safety)
- **Graph integrity is enforced** (no missing task/agent links)
- **Dependencies are explicit** (scope/ordering/requirements are types, not options)
- **Serialization mirrors domain reality** (clear semantics)
- **Call sites are forced to decide** (no silent None paths)

**Rule of Thumb:**

If a field encodes **structure or dependency**, use a **discriminated union**.  
If a value is **required by design**, do not wrap it in `Option`.  
If an invariant must hold, make the **invalid state unrepresentable**.

---

### ✅ Pattern: Newtype Wrapper for Domain Concepts

**The Problem:**

Primitive types (String, i64, f64, UUID) used for domain concepts create subtle bugs that compile but fail at runtime or cause logic errors. The type system can't distinguish between primitives with different meanings.

**Core Principle:**

If two values have different **meanings** or **invariants** in your domain, they should have different **types** - even if they have the same representation. This is an extension of "use strong types at boundaries" applied throughout your domain model.

**Why This Matters:**

```rust
// ❌ Primitives everywhere - compiler can't help
fn calculate_total(price_cents: i64, tax_cents: i64, fee_cents: i64) -> i64 {
    price_cents + tax_cents + fee_cents
}

// Caller accidentally swaps fee and tax - compiles fine, wrong result!
let total = calculate_total(1000, 50, 80);  // Which is which? Who knows!

// ✅ Newtypes - compiler enforces correctness
fn calculate_total(price: UsdCents, tax: UsdCents, fee: UsdCents) -> UsdCents {
    UsdCents(price.0 + tax.0 + fee.0)
}

// Caller can't mess this up - parameter names ARE the types!
let total = calculate_total(
    UsdCents(1000),  // price
    UsdCents(80),    // tax
    UsdCents(50),    // fee
);
```

**The Pattern:** Wrap primitives in single-field structs to give them domain meaning.

**Example 1: Currencies and Amounts**

**Bad (Primitive Confusion):**

```rust
// ❌ All money is just i64 - compiler can't catch currency mistakes!
pub async fn convert_currency(
    amount: i64,           // What currency? Who knows!
    from_rate: f64,        // Rate for what currency pair?
    to_rate: f64,          // Is this inverted? Nobody can tell!
) -> i64 {
    // OOPS! Mixed up from/to rates - compiles, wrong result!
    (amount as f64 * to_rate / from_rate) as i64
}

pub async fn transfer(
    usd_amount: i64,       // Caller thinks this is USD...
    eur_amount: i64,       // ...and this is EUR
) {
    // OOPS! Swapped parameters - compiles fine, catastrophic loss!
    let total = usd_amount + eur_amount;  // Adding USD + EUR like they're the same!
}

// More horror:
pub async fn calculate_fee(amount: i64, rate: f64) -> i64 {
    (amount as f64 * rate) as i64
}

// Caller confusion:
let amount_cents = 1000;        // $10.00
let tax_rate = 0.08;            // 8%
let fee = calculate_fee(amount_cents, tax_rate);  // Compiles!

let amount_dollars = 10.0;
let basis_points = 50.0;        // 0.5% as 50 bps
let fee2 = calculate_fee(amount_dollars as i64, basis_points);  // WRONG! But compiles!
```

**Good (Newtype Pattern):**

```rust
// ✅ Each currency and unit is a distinct type!

/// USD amount in cents (eliminates floating point errors)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsdCents(pub i64);

/// EUR amount in cents
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EurCents(pub i64);

/// Percentage as basis points (1 bp = 0.01%, eliminates float errors)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BasisPoints(pub i64);

/// Tax rate as rational (numerator/denominator for exact math)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaxRate {
    pub numerator: i64,
    pub denominator: i64,
}

impl UsdCents {
    pub fn apply_basis_points(&self, bp: BasisPoints) -> Self {
        // ✅ Correct formula, type-safe!
        Self((self.0 * bp.0) / 10_000)
    }

    pub fn apply_tax(&self, rate: TaxRate) -> Self {
        // ✅ Exact integer math, no floating point errors!
        Self((self.0 * rate.numerator) / rate.denominator)
    }
}

// ✅ Type-safe API - impossible to mix currencies!
pub async fn transfer(
    usd_amount: UsdCents,
    eur_amount: EurCents,
) {
    // ✅ Compiler error: can't add UsdCents + EurCents!
    // let total = usd_amount + eur_amount;  // Won't compile!

    // Must explicitly convert:
    let eur_as_usd = convert_to_usd(eur_amount).await?;
    let total = UsdCents(usd_amount.0 + eur_as_usd.0);  // ✅ Safe!
}

pub async fn convert_to_usd(eur: EurCents) -> Result<UsdCents, Error> {
    let rate = get_eur_usd_rate().await?;
    Ok(UsdCents((eur.0 * rate.numerator) / rate.denominator))
}

// ✅ Self-documenting function signatures
pub async fn calculate_fee(amount: UsdCents, rate: BasisPoints) -> UsdCents {
    amount.apply_basis_points(rate)
}

// Caller clarity:
let amount = UsdCents(1000);           // $10.00
let fee_rate = BasisPoints(50);        // 0.5%
let fee = calculate_fee(amount, fee_rate);  // ✅ Types enforce correctness!

// This won't compile - type mismatch caught at compile time!
// let tax = TaxRate { numerator: 8, denominator: 100 };
// let fee2 = calculate_fee(amount, tax);  // ❌ Compiler error!
```

**Example 2: IDs and References**

**Bad (Primitive ID Hell):**

```rust
// ❌ All IDs are just UUIDs - compiler can't help you!
pub async fn transfer(
    from_account: Uuid,
    to_account: Uuid,
    user_id: Uuid,
    correlation_id: Uuid,
) -> Result<Uuid, Error> {
    // OOPS! Swapped account_id and user_id - compiles fine, breaks at runtime!
    lock_account(user_id).await?;

    // OOPS! Used correlation_id instead of to_account - logic bug!
    credit_account(correlation_id, amount).await?;

    Ok(from_account)  // OOPS! Returned wrong ID - compiles, wrong semantics!
}

// More horror: mixing internal IDs with external refs
pub async fn process_payment(
    from: String,  // Is this external_ref or account_id.to_string()?
    to: String,    // Is this external_ref or account_id.to_string()?
) -> Result<(), Error> {
    // Nobody knows! Runtime errors ahead!
    let from_uuid = Uuid::parse_str(&from)?;  // Might be wrong type entirely!
}

// Database operations become error-prone
pub async fn lock_account(account_id: Uuid) {
    // Uses account_id for advisory lock
    let lock_key = (account_id.as_u128() & 0x7FFF_FFFF_FFFF_FFFF) as i64;
}

pub async fn resolve_external_ref(external_ref: &str) -> Uuid {
    // Returns account_id UUID
}

// Caller confusion:
let external_ref = "user-123";
let account_id = resolve_external_ref(external_ref).await?;
lock_account(external_ref)?;  // ❌ WRONG! But if external_ref was UUID string, compiles!
```

**Good (Newtype Pattern):**

```rust
// ✅ Each ID type is distinct - compiler enforces correctness!

/// Internal UUID for database primary keys and advisory locks
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AccountId(pub Uuid);

/// External human-readable reference for API operations
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExternalRef(pub String);

/// User identity separate from account identity
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UserId(pub Uuid);

/// Correlation ID for tracking related transactions
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CorrelationId(pub Uuid);

impl std::fmt::Display for AccountId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::fmt::Display for ExternalRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ✅ Type-safe API - impossible to mix up IDs!
pub async fn transfer(
    from_account: AccountId,
    to_account: AccountId,
    user_id: UserId,
) -> Result<CorrelationId, Error> {
    // ✅ Compiler error if you try to pass wrong ID type!
    lock_account(from_account).await?;

    // ✅ Can't accidentally use correlation_id as account_id!
    credit_account(to_account, amount).await?;

    // ✅ Return type is explicit
    Ok(correlation_id)
}

// ✅ Clear separation of external vs internal IDs
pub async fn process_payment(
    from_external_ref: &ExternalRef,
    to_external_ref: &ExternalRef,
) -> Result<CorrelationId, Error> {
    // ✅ Explicit conversion with clear semantics
    let from_account = resolve_external_ref(from_external_ref).await?;
    let to_account = resolve_external_ref(to_external_ref).await?;

    transfer(from_account, to_account).await
}

// ✅ Function signatures document intent
pub async fn lock_account(account_id: AccountId) {
    // Uses account_id.0 for advisory lock
    let lock_key = (account_id.0.as_u128() & 0x7FFF_FFFF_FFFF_FFFF) as i64;
}

pub async fn resolve_external_ref(external_ref: &ExternalRef) -> Result<AccountId, Error> {
    // Returns AccountId - clear and type-safe!
}
```

**Critical Benefits:**

1. **Prevents Domain Logic Bugs:**

   - Can't add USD + EUR amounts (different currencies)
   - Can't pass `user_id` where `account_id` is expected (different entities)
   - Can't apply tax rate where basis points expected (different units)
   - Can't swap `from` and `to` parameters silently (compiler catches it)

2. **Self-Documenting Code:**

   - Function signatures show exact domain semantics: `fn transfer(UsdCents, EurCents)`
   - No need to guess if `String` is email, username, or external ref
   - Clear distinction between representations: UUID vs ExternalRef, Cents vs Dollars
   - Method names encode domain operations: `apply_basis_points()`, `lock_account()`

3. **Refactor Safety:**

   - Changing `AccountId` from UUID to i64? Compiler finds all usages!
   - Changing `UsdCents` from i64 to i128? Compiler catches all conversions!
   - Adding validation? Single place to add it in the newtype's constructor!
   - Representation changes don't break API contracts

4. **Eliminates Entire Bug Classes:**
   - No currency confusion (Mars Climate Orbiter-style bugs)
   - No unit confusion (mixing meters/feet, seconds/milliseconds)
   - No ID type confusion (advisory locks, database operations)
   - No rate/percentage confusion (basis points vs percentages vs decimals)

**When to Use Newtypes:**

- ✅ **Always** for values with different domain meanings (UsdCents vs EurCents, Email vs Username)
- ✅ **Always** for values with different invariants (NonZeroU64 vs u64, PositiveAmount vs Amount)
- ✅ **Always** for IDs representing different entities (AccountId, UserId, OrderId, CorrelationId, TransactionId)
- ✅ **Always** when mixing internal/external representations (UUID vs ExternalRef, Cents vs Dollars)
- ✅ **Always** for values used in operations with side effects (locks, deletes, transfers)
- ✅ **Always** in public APIs (prevents caller mistakes)
- ✅ **Always** for units of measure (Meters vs Feet, Seconds vs Milliseconds)
- ✅ **Always** at serialization boundaries (HTTP requests/responses, database rows, message queues)
- ✅ **Always** for pagination cursors with different structures (BalanceCursor vs TransactionCursor)

**When Primitives Are Acceptable:**

- Internal helper functions with very limited scope and single obvious meaning
- Temporary variables within same function (still risky!)
- Values that truly have no domain semantics (array indices, loop counters)
- Performance-critical hot paths (only after benchmarking proves need)

**Common Pitfalls:**

```rust
// ❌ Newtype but still exposing raw type
pub struct AccountId(pub Uuid);  // pub inner field!

pub fn process(account: AccountId, other_uuid: Uuid) {
    // Caller can do: process(AccountId(other_uuid), account.0)
    // Defeats the purpose!
}

// ✅ Better: methods instead of public fields
pub struct AccountId(Uuid);  // private!

impl AccountId {
    pub fn new(uuid: Uuid) -> Self {
        Self(uuid)
    }

    pub fn as_uuid(&self) -> Uuid {
        self.0
    }

    // For internal/DB use only
    pub(crate) fn inner(&self) -> Uuid {
        self.0
    }
}

// ❌ Implementing From/Into too loosely
impl From<Uuid> for AccountId {
    fn from(uuid: Uuid) -> Self {
        Self(uuid)
    }
}

// Now any Uuid can become AccountId silently - defeats purpose!
let oops: AccountId = some_random_uuid.into();

// ✅ Better: explicit constructor only
impl AccountId {
    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }
}

// Caller must explicitly: AccountId::from_uuid(uuid)
```

**Real-World Example (from our codebase):**

```rust
// Before (primitive types - bugs waiting to happen):
pub async fn credit_from_system(account_id: Uuid, ...) -> Result<Uuid, Error> {
    validate_not_system_account(account_id)?;
    let lock_key = (account_id.as_u128() & 0x7FFF) as i64;
    // ...
}

pub async fn get_account_by_external_ref(external_ref: &str) -> Result<Option<Uuid>, Error> {
    // Returns UUID - but is it account_id? user_id? Nobody knows from type!
}

// Caller confusion:
let uuid = get_account_by_external_ref("user-123").await?;
credit_from_system(uuid, ...).await?;  // Compiles, might be wrong UUID type!

// After (newtypes - compiler enforces correctness):
pub async fn credit_from_system(account_id: AccountId, ...) -> Result<CorrelationId, Error> {
    validate_not_system_account(account_id)?;
    let lock_key = (account_id.0.as_u128() & 0x7FFF) as i64;
    // ...
}

pub async fn get_account_by_external_ref(
    external_ref: &ExternalRef
) -> Result<Option<AccountId>, Error> {
    // ✅ Return type is explicit and type-safe!
}

// Caller clarity:
let account_id = get_account_by_external_ref(&ExternalRef("user-123".into()))
    .await?
    .ok_or(Error::AccountNotFound)?;

credit_from_system(account_id, ...).await?;  // ✅ Types enforce correctness!
```

---

## API Design

### ✅ Pattern: Type Aliases for Clarity

**Bad:**

```rust
pub async fn get_balance(...)
    -> Result<Json<Balance>, (StatusCode, Json<serde_json::Value>)>  // ❌ Repeated everywhere
{
    // ...
}
```

**Good:**

```rust
type HttpResult<T> = Result<Json<T>, (StatusCode, Json<serde_json::Value>)>;

pub async fn get_balance(...) -> HttpResult<Balance> {  // ✅ Clear and concise
    // ...
}
```

### ✅ Pattern: Helper Functions for Common Operations

**Bad:**

```rust
let metadata = req.metadata.unwrap_or_else(|| serde_json::json!({}));  // ❌ Repeated
```

**Good:**

```rust
fn default_metadata() -> serde_json::Value {
    serde_json::json!({})
}

let metadata = req.metadata.unwrap_or_else(default_metadata);  // ✅ DRY
```

---

## Enum Handling at Boundaries

### ✅ Pattern: Schema-Qualified Enums - Use ::text + FromStr

**The Problem with sqlx::Type for Schema-Qualified Enums:**

PostgreSQL strips schema qualification from column type metadata in query results. This causes a fundamental mismatch:

- **Encoding (INSERT):** Rust says `credit_accounting.transaction_type` → binds correctly
- **Decoding (SELECT):** PostgreSQL reports column as just `transaction_type` (no schema!) → **MISMATCH!**

Error you'll see:

```
mismatched types; Rust type (as SQL type `credit_accounting.transaction_type`)
is not compatible with SQL type `transaction_type`
```

**For enums in the public schema (no qualification):**

Use `sqlx::Type` derive - works fine since no schema mismatch:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "order_status", rename_all = "lowercase")]
pub enum OrderStatus {
    Pending,
    Completed,
    Cancelled,
}

// Direct usage - SQLx handles everything
let status: OrderStatus = row.try_get("status")?;  // ✅
sqlx::query("INSERT INTO orders (status) VALUES ($1)")
    .bind(OrderStatus::Pending)  // ✅
    .execute(&pool).await?;
```

**For enums in non-public schemas (use ::text + FromStr):**

PostgreSQL strips schema qualification from column metadata in query results, breaking `sqlx::Type` decode.

```rust
// NO sqlx::Type! Use FromStr + as_str() pattern instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionType { Credited, Paid, WithdrawalRequested }

impl FromStr for TransactionType {
    type Err = TransactionTypeParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "credited" => Ok(Self::Credited),
            "paid" => Ok(Self::Paid),
            // ... exhaustive match
            _ => Err(TransactionTypeParseError { input: s.to_string() }),
        }
    }
}

impl TransactionType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Credited => "credited",
            Self::Paid => "paid",
            // ... symmetric with FromStr
        }
    }
}

// INSERT: Bind string + explicit schema cast
sqlx::query("INSERT INTO ... VALUES ($1::my_schema.my_enum)")
    .bind(value.as_str())  // ✅
    .execute(&pool).await?;

// SELECT: Cast to text + parse
let row = sqlx::query("SELECT my_enum::text as my_enum FROM ...").fetch_one(&pool).await?;
let value: MyEnum = row.try_get::<String, _>("my_enum")?.parse()?;  // ✅
```

**Principle:** Schema-qualified enums require `::text` + `FromStr` pattern despite PR #3252 in SQLx 0.8.6.

**Note:** While [SQLx PR #3252](https://github.com/launchbadge/sqlx/pull/3252) (merged June 2024) aimed to fix schema-qualified enum support, testing with SQLx 0.8.6 shows it doesn't fully resolve the issue. The `::text` + `FromStr` pattern remains the reliable approach. When future SQLx versions properly support schema-qualified enums, migration will be straightforward (remove `::text` casts and use `#[derive(sqlx::Type)]`).

---

### ✅ Pattern: Custom Type Conversions with SQLx Traits

For newtypes requiring conversion logic (e.g., i128 ↔ NUMERIC), implement `Type`, `Encode`, and `Decode`. Delegate to intermediate types that SQLx already supports (like `BigDecimal`).

**Principle:** Implement traits once at the boundary, then use the type naturally everywhere - no manual conversion needed.

**See:** `lib/credit-accounting/src/types.rs` (Amount type) for complete implementation.

---

### ✅ Pattern: Implement FromStr for Database Enums

Required for schema-qualified enums (and good practice for all database enums).

**Correct FromStr Implementation:**

```rust
// 1. Define a proper error type (thiserror makes this easy)
#[derive(Debug, Clone, thiserror::Error)]
#[error("invalid order status: {input}")]
pub struct OrderStatusParseError {
    pub input: String,
}

// 2. Implement FromStr with proper error type
impl std::str::FromStr for OrderStatus {
    type Err = OrderStatusParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "completed" => Ok(Self::Completed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(OrderStatusParseError {
                input: s.to_string(),
            }),
        }
    }
}

// 3. Add proper error variant (using #[from] for auto-conversion)
#[derive(Debug, Error)]
pub enum MyError {
    // ...
    #[error("order status parse error: {0}")]
    OrderStatusParse(#[from] OrderStatusParseError),
    // ...
}

// 4. Clean parsing in code (automatic error conversion via ?)
let status_str: String = row.try_get("status")?;
let status: OrderStatus = status_str.parse()?;  // ✅ Idiomatic and correct!
```

**Summary - When to Use Each Pattern:**

| Enum Location     | Pattern                                    | Example                              |
| ----------------- | ------------------------------------------ | ------------------------------------ |
| Public schema     | `#[derive(sqlx::Type)]`                    | `order_status` enum                  |
| Non-public schema | `::text` + `FromStr` + `as_str()`          | `credit_accounting.transaction_type` |
| Either            | Implement `FromStr` with proper error type | Always recommended                   |

---

## Database Interactions

### ✅ Pattern: Proper Row Extraction

**Bad:**

```rust
let rows: Vec<Balance> = sqlx::query(...)
    .fetch_all(&pool)
    .await?
    .into_iter()
    .map(|row| {
        Balance {
            id: row.try_get("id").unwrap(),  // ❌ PANIC on schema mismatch!
            amount: row.try_get("amount").unwrap(),
        }
    })
    .collect();
```

**Good:**

```rust
let rows: Result<Vec<Balance>, MyError> = sqlx::query(...)
    .fetch_all(&pool)
    .await?
    .into_iter()
    .map(|row| {
        Ok(Balance {
            id: row.try_get("id")
                .map_err(|e| MyError::RowExtraction(format!("id: {}", e)))?,
            amount: row.try_get("amount")
                .map_err(|e| MyError::RowExtraction(format!("amount: {}", e)))?,
        })
    })
    .collect();

let rows = rows?;  // Propagate error properly
```

---

## Logging

### ✅ Pattern: Structured Logging with Static Messages

**Bad:**

```rust
tracing::info!("Processing payment from {} to {} for {}", from, to, amount);  // ❌ Unstructured
```

**Good:**

```rust
tracing::info!(
    from = %from,
    to = %to,
    amount = amount,
    event = "payment_processing"  // ✅ Structured, filterable
);
```

**Rules:**

- Message strings must be static (no `format!` or interpolation)
- All dynamic data in fields
- Use `%` for Display, `?` for Debug
- Add `event` field for filtering

---

## Lint and Hygiene

### ❌ Anti-Pattern: `#[allow(dead_code)]` Without Justification

**In production Rust, `#[allow(dead_code)]` is always a smell.** It suppresses the compiler’s signal that something is unused. Prefer removing the code or using it; only allow when the use is explicit and documented.

**Bad:**

```rust
#[allow(dead_code)]
pub struct Config {
    pub retries: u32,
    legacy_timeout_ms: u64,  // ❌ Unused; allow hides it
}

#[allow(dead_code)]
fn helper() { }  // ❌ Never called; delete or use it
```

**Good:**

```rust
// ✅ No allow: remove unused code or use it
pub struct Config {
    pub retries: u32,
    pub timeout_ms: u64,  // Used at call sites
}

// ✅ If code is intentionally reserved for future/protocol use, document why
/// Reserved for protocol extensibility (e.g. cancellation reason). In production,
/// allow(dead_code) is a smell; prefer removing or using the code.
#[allow(dead_code)]
pub enum ToolSessionOp {
    Open { reason: Option<String>, ... },
    ...
}
```

**Rules:**

- Prefer **removing** unused code or **using** it.
- If you keep `#[allow(dead_code)]`, add a **comment** that in production this is a smell and why the code is reserved (e.g. protocol field, future use).
- Revisit allows periodically; remove when the code is used or deleted.

---

## Checklist: Production-Ready Code

Before shipping, verify:

- [ ] **No unwrap/expect** in production paths (only in tests or with justification)
- [ ] **No `#[allow(dead_code)]`** without a comment that it’s a smell and why the code is reserved
- [ ] **Strong types** at API boundaries (no String for amounts/IDs)
- [ ] **Invalid states unrepresentable** (use discriminated unions for structure/deps)
- [ ] **Proper error handling** with context (which field/operation failed)
- [ ] **Structured logging** (static messages, dynamic data in fields)
- [ ] **Type aliases** for complex signatures (DRY)
- [ ] **Helper functions** for repeated patterns
- [ ] **Row extraction** with proper error handling
- [ ] **No version history in comments** (present tense, current behavior only)
- [ ] **TrackedTransaction** for conditional transaction paths (prevents empty commit warnings)

---

## Comments and Documentation

### ❌ Anti-Pattern: Version History in Comments

**Bad:**

```rust
// Migrations are now handled centrally by database_support::apply_all_migrations()
// (Removed) helper-based root span; tests will use axum-tracing-opentelemetry middleware instead.
// This was changed from the old approach to the new approach
// Updated to use the centralized system
```

**Good:**

```rust
// Migrations handled centrally by database_support::apply_all_migrations()
// Centralized migration management ensures proper ordering
// Axum-tracing-opentelemetry middleware provides root span functionality
```

**Why This Matters:**

- Comments should describe **current behavior**, not historical changes
- Version history belongs in git commits, not code comments
- Past-tense comments become outdated and confusing over time
- Comments should explain **what** and **why**, not **what changed**

**Rules:**

- ✅ Use present tense: "Migrations handled centrally"
- ✅ Describe current state: "Helper-based root span removed"
- ✅ Explain current purpose: "Centralized migration management ensures proper ordering"
- ❌ Avoid past tense: "Migrations are now handled"
- ❌ Avoid change documentation: "This was changed from X to Y"
- ❌ Avoid version history: "Updated to use the new system"

---

## Database Transactions

### ✅ Pattern: Track Operations to Prevent Empty Transaction Commits

**The Problem:**

PostgreSQL warns when committing transactions that contain no write operations (INSERT, UPDATE, DELETE). This commonly occurs when:

- Transactions are started for conditional operations
- Early returns happen before any writes occur
- Read-only transactions are accidentally committed

**Bad:**

```rust
// ❌ Transaction started but may be committed with no operations
let mut tx = pool.begin().await?;

if should_skip {
    return Ok(());  // Transaction dropped, but if it was committed, PostgreSQL warns!
}

// ... perform operations ...
sqlx::query("COMMIT").execute(&mut tx.executor()).await?;
```

**Good (Using TrackedTransaction):**

```rust
use database_support::tracked_transaction::TrackedTransaction;

// ✅ Track whether operations occurred
let tx = pool.begin().await?;
let mut tracked = TrackedTransaction::new(tx);

if should_skip {
    // Explicitly rollback before returning
    tracked.rollback().await?;
    return Ok(());
}

// Perform write operations
sqlx::query("INSERT INTO ...").execute(tracked.as_mut().executor()).await?;
tracked.mark_operation();  // Mark that a write occurred

// Automatically commits if operations occurred, rolls back if empty
tracked.commit_if_needed().await?;
```

**Pattern for Conditional Operations:**

```rust
async fn conditional_operation(
    pool: &sqlx_tracing::Pool<Postgres>,
) -> Result<(), Error> {
    let tx = pool.begin().await?;
    let mut tracked = TrackedTransaction::new(tx);

    // Check conditions that might skip operations
    if !should_perform_operation() {
        tracked.rollback().await?;
        return Ok(());
    }

    // Perform write operations
    sqlx::query("INSERT INTO ...").execute(tracked.as_mut().executor()).await?;
    tracked.mark_operation();

    // Commit only if operations occurred
    tracked.commit_if_needed().await?;
    Ok(())
}
```

**Pattern for Lock-Then-Check Operations:**

```rust
async fn lock_and_check(
    pool: &sqlx_tracing::Pool<Postgres>,
    account_id: Uuid,
    amount: i128,
) -> Result<(), Error> {
    let tx = pool.begin().await?;
    let mut tracked = TrackedTransaction::new(tx);

    // Acquire lock (this is a write operation in PostgreSQL)
    sqlx::query("SELECT pg_advisory_xact_lock(...)")
        .execute(tracked.as_mut().executor())
        .await?;

    // Check balance
    let balance: i128 = sqlx::query_scalar("SELECT balance FROM ...")
        .fetch_one(tracked.as_mut().executor())
        .await?;

    if balance < amount {
        // Rollback before returning error (no writes occurred)
        tracked.rollback().await?;
        return Err(Error::InsufficientBalance);
    }

    // Perform write operations
    sqlx::query("INSERT INTO ...").execute(tracked.as_mut().executor()).await?;
    tracked.mark_operation();

    // Commit if operations occurred
    tracked.commit_if_needed().await?;
    Ok(())
}
```

**When to Use TrackedTransaction:**

- ✅ **Always** when transactions may have conditional early returns
- ✅ **Always** when transactions are started before knowing if operations will occur
- ✅ **Always** in test data generation code with conditional paths
- ✅ **Always** when checking conditions before performing writes

**When TrackedTransaction is Not Needed:**

- ❌ Transactions that always perform writes (no conditional paths) - though using it is still safe
- ❌ Read-only transactions (use `ROLLBACK` explicitly or don't start a transaction)
- ⚠️ Transactions that must always commit (e.g., for advisory lock release) - use `commit_unconditionally()` instead

**Benefits:**

- **Eliminates PostgreSQL warnings** about empty transaction commits
- **Explicit intent** - code clearly shows when operations may not occur
- **Automatic handling** - `commit_if_needed()` handles commit vs rollback logic
- **Type safety** - compiler enforces marking operations before commit

**Implementation Details:**

The `TrackedTransaction` wrapper:

- Tracks a boolean flag indicating if write operations occurred
- Provides `mark_operation()` to indicate a write happened
- Provides `commit_if_needed()` which commits if operations occurred, otherwise rolls back
- Provides `commit_unconditionally()` for cases where commit is required (e.g., lock release)
- Provides `rollback()` for explicit rollback on errors

**See:** `lib/database-support/src/tracked_transaction.rs` for the full implementation.

---

## Devmode and Fakes Crate

### ✅ Pattern: Explicit Fakes Crate for Devmode Features

**The Problem:**

Devmode features (mock servers, testcontainers, fake implementations) were previously scattered across multiple crates with complex feature gating. This created risk of accidentally deploying fake systems to production.

**The Solution:**

All devmode features, mocks, and fake implementations are consolidated into a single `fakes` crate that is explicitly named and documented as containing fake implementations.

**Bad (Scattered Devmode Features):**

```rust
// ❌ Devmode features scattered across crates
#[cfg(feature = "devmode")]
pub mod mocks { /* ... */ }

#[cfg(feature = "devmode")]
pub mod devmode { /* ... */ }

// ❌ Complex feature gating makes it easy to miss production safety
#[cfg(any(feature = "devmode", feature = "test-helpers"))]
pub fn ensure_postgres() { /* ... */ }
```

**Good (Centralized Fakes Crate):**

```rust
// ✅ All fakes in one clearly-labeled crate
// lib/fakes/src/lib.rs
//! ⚠️ **WARNING: THIS CRATE CONTAINS FAKE IMPLEMENTATIONS FOR DEVELOPMENT ONLY**
//! **DO NOT USE IN PRODUCTION**

pub mod mock_servers;  // Privy/Google mock servers
pub mod test_containers;  // Postgres/Anvil testcontainers
pub mod routes;  // Devmode HTTP routes
pub mod bootstrap;  // Devmode data generation

// ✅ Explicit dependency in Cargo.toml
[features]
devmode = ["fakes"]  # Single, clear dependency

[dependencies]
# Production dependencies only
credit_accounting = { path = "../credit-accounting" }

[dev-dependencies]
# Tests can use fakes directly
fakes = { path = "../fakes" }
```

**Critical Rules:**

1. **Never include `fakes` in production dependencies:**

   ```toml
   # ❌ BAD - fakes in production dependencies
   [dependencies]
   fakes = { path = "../fakes" }

   # ✅ GOOD - fakes only in devmode feature or dev-dependencies
   [features]
   devmode = ["fakes"]

   [dev-dependencies]
   fakes = { path = "../fakes" }
   ```

2. **Always use feature gating for devmode code:**

   ```rust
   // ✅ GOOD - explicit feature gate
   #[cfg(feature = "devmode")]
   {
       let mock_env = fakes::ensure_privy_mock().await?;
       // ...
   }
   ```

3. **Document fakes usage clearly:**
   ```rust
   // ✅ GOOD - clear documentation
   /// Bootstrap devmode environment with sample data.
   ///
   /// ⚠️ **WARNING: This generates FAKE data for development only. DO NOT USE IN PRODUCTION.**
   pub async fn bootstrap_devmode(...) -> Result<()> {
       // ...
   }
   ```

**Benefits:**

- **Clear separation**: Production code cannot accidentally include fakes
- **Simplified feature gating**: Single dependency instead of scattered feature flags
- **Production safety**: Compiler prevents inclusion without explicit dependency
- **Better organization**: All devmode/test code in one place
- **Self-documenting**: Crate name and documentation clearly indicate fake implementations

**When to Use Fakes Crate:**

- ✅ **Always** for mock servers (Privy, Google JWKS)
- ✅ **Always** for testcontainers (Postgres, Anvil)
- ✅ **Always** for devmode HTTP routes (token generation)
- ✅ **Always** for devmode bootstrap (test data generation)
- ✅ **Always** for fake secret derivation (devmode/test environments)
- ✅ **Always** for mock trait implementations (testing)

**When NOT to Use Fakes Crate:**

- ❌ **Never** in production dependencies
- ❌ **Never** in production code paths (always behind `#[cfg(feature = "devmode")]`)
- ❌ **Never** for production implementations (use real services)

**See:** `lib/fakes/README.md` for complete documentation of the fakes crate.

---

## When to Break These Rules

**Never:**

- Unwrapping fallible operations without explicit validation
- Ignoring errors silently
- Recording version history in comments

**Rarely (with comment):**

- Manual parsing in hot paths (only after benchmarking proves need)
- Inline instrumentation (only for very tight coupling)

**Document why** when you deviate from these patterns.
