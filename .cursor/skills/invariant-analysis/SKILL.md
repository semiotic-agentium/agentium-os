---
name: invariant-analysis
description: Identify invariant properties of subsystems or modules, describe them in semi-formal notation, and document them to inform code comments and testing. Use when analyzing code for properties that must always hold, when writing property tests, or when documenting system guarantees.
---

# Invariant Analysis

This skill guides you through identifying, formalizing, and documenting invariant properties of subsystems or modules using semi-formal mathematical notation.

## Quick Start

When analyzing a subsystem for invariants:

1. **Discover** invariants using the discovery process below
2. **Formalize** each invariant in semi-formal notation
3. **Document** with Property, Enforcement, and Testing sections
4. **Encode** in property tests to verify they always hold

## Discovery Process

Follow these steps to identify invariants in a subsystem:

### 1. Start from the Contract

Walk the public API surface and ask: _what must be true before and after every call?_

Look for:

- **Idempotency guarantees**: Same operation called twice produces same result
- **Monotonic counters**: Values that only increase or decrease
- **Atomic state transitions**: Operations that must succeed completely or not at all
- **Relationship consistency**: Entities that must maintain specific relationships

**Example:**

```rust
// Public API: enqueue(item)
// Invariant: queue.size() < MAX_SIZE (before enqueue)
// Invariant: queue.size() == old_size + 1 (after enqueue)

// Public API: dequeue()
// Invariant: queue.size() > 0 (before dequeue)
// Invariant: queue.size() == old_size - 1 (after dequeue)
```

### 2. Trace Data Flow Across Boundaries

Follow a request through layers (HTTP handlers → service logic → stores → database).

Mismatches between layers expose invariants:

- "The system must never create duplicate records for the same identifier"
- "State changes must be atomic across related tables"
- "All layers must agree on validation rules"

### 3. Interrogate Failure Modes

Review:

- Postmortems and incident reports
- TODO comments mentioning edge cases
- Error handling paths

If a past outage involved duplicate operations, race conditions, or inconsistent state, capture the prevention rule as an invariant.

### 4. Consider Conservation and Exclusivity

Look for:

- **Conserved quantities**: Totals, counts, resources that must balance
- **Exclusive access**: Only one operation per resource, single-writer guarantees
- **Constraints**: Bounds, relationships, state machine rules that must never be violated

**Example:**

```rust
// Cache: Σ(size of all entries) <= MAX_CACHE_SIZE (conservation)
// Task queue: Only one worker processes task.id at a time (exclusivity)
// Counter: counter.value >= 0 (constraint)
```

### 5. Move Between Scopes

Some invariants live at:

- **Single-operation level**: Atomicity, validation
- **Batch scope**: Aggregate consistency
- **System scope**: Global constraints

Write tests at both scopes to catch regressions.

## Semi-Formal Notation

Use mathematical notation mixed with English for clarity:

### Common Symbols

| Symbol                | Meaning              | Example                                                       |
| --------------------- | -------------------- | ------------------------------------------------------------- |
| `∀`                   | For all              | `∀ item ∈ cache: item.size <= MAX_ITEM_SIZE`                  |
| `Σ`                   | Summation            | `Σ(entry.size) <= MAX_CACHE_SIZE`                             |
| `∃`                   | There exists         | `∃ exactly one record WHERE id = x`                           |
| `XOR`                 | Exclusive or         | `(field1 > 0 AND field2 = 0) XOR (field1 = 0 AND field2 > 0)` |
| `∴`                   | Therefore            | `total = cached + pending ∴ total = SUM(all operations)`      |
| `∈`                   | Element of           | `status ∈ {PENDING, CONFIRMED, FAILED}`                       |
| `>=`, `<=`, `=`, `!=` | Comparison operators | `count >= 0`                                                  |

### Notation Patterns

**Universal quantification:**

```
∀ operation op: pre_condition(op) → post_condition(op)
```

**Existence and uniqueness:**

```
∀ key k: EXISTS at most one record WHERE id = k
```

**State transitions:**

```
∀ entity e WHERE e.status ∈ {TERMINAL_STATES}:
  e.status cannot transition to any other state
```

**Conservation:**

```
Σ(ALL resources) = initial_total (nothing created/destroyed)
```

**Concurrency:**

```
∀ operation op, ∀ N concurrent executions:
  At most one execution succeeds
```

**Bounded quantities:**

```
∀ time t: queue.size(t) <= MAX_QUEUE_SIZE
```

## Documentation Structure

Document each invariant with this structure:

```markdown
### N. [Invariant Name]

**Property:**
```

[Semi-formal notation]

```

[English explanation of what the invariant means]

**Enforcement:**

| Layer | Mechanism |
|-------|-----------|
| **Application** | [How code enforces it] |
| **Database** | [How DB constraints enforce it] |
| **Testing** | [How tests verify it] |
```

### Example Documentation

```markdown
### 1. Cache Size Invariant

**Property:**
```

Σ(entry.size WHERE entry ∈ cache) <= MAX_CACHE_SIZE

```

The total size of all cached entries must never exceed the maximum cache size.

**Enforcement:**

| Layer           | Mechanism                                                 |
| --------------- | --------------------------------------------------------- |
| **Application** | `Cache::insert()` checks total size before adding entry   |
| **Eviction**     | LRU eviction runs when size approaches limit              |
| **Testing**      | `prop_cache_size_invariant_holds` property test          |
```

**Another example:**

```markdown
### 2. State Machine Transition Invariant

**Property:**
```

∀ entity e, ∀ state s:
IF e.status = s AND s ∈ TERMINAL_STATES
THEN e.status cannot transition to any other state

```

Once an entity reaches a terminal state, it cannot transition further.

**Enforcement:**

| Layer           | Mechanism                                                 |
| --------------- | --------------------------------------------------------- |
| **Application** | `StateMachine::transition()` validates against state graph |
| **Type System**  | Enum variants prevent invalid state assignments            |
| **Testing**      | `prop_terminal_state_invariant_holds` property test       |
```

## Code Comments

When documenting invariants in code:

### Module-Level Documentation

```rust
//! Property-based tests for task_queue.
//!
//! ## Core Properties (The Laws That Cannot Be Broken)
//!
//! 1. **Queue Size Bound**: ∀ time t: queue.size(t) <= MAX_QUEUE_SIZE
//! 2. **Task Uniqueness**: ∀ task_id: EXISTS at most one task WHERE id = task_id
//! 3. **FIFO Ordering**: ∀ tasks t1, t2: IF t1.enqueued_at < t2.enqueued_at THEN t1.dequeued_at <= t2.dequeued_at
//! 4. **Non-Negative Count**: ∀ time t: queue.size(t) >= 0
```

### Function-Level Documentation

```rust
/// **Invariant:**
///
/// ∀ operation_id, ∀ concurrent executions:
///   At most one execution succeeds
///
/// This function ensures idempotency by checking for existing records
/// before creating new ones.
pub async fn process_operation(operation_id: &str) -> Result<()> {
    // ...
}
```

### Inline Comments

```rust
// INVARIANT: Resource conservation (checked atomically via single query)
// Σ(ALL allocated_resources) = TOTAL_AVAILABLE
let total: i64 = query_scalar(&query).await?;
prop_assert_eq!(total, TOTAL_AVAILABLE, "RESOURCE CONSERVATION VIOLATED");
```

## Property Test Encoding

Encode invariants as property tests using proptest:

### Test Structure

```rust
/// PROPERTY N: [Invariant Name]
///
/// [Semi-formal notation]
///
/// [Explanation of what the test verifies]
#[proptest]
fn prop_invariant_name() {
    proptest!(|(operations in arb_operations())| {
        let system = setup_test_system().await;

        // Apply operations
        for op in operations {
            system.apply(op).await?;
        }

        // Verify invariant
        prop_assert!(verify_invariant(&system).await?);
    });
}
```

### Helper Functions

Wrap complex invariant checks in reusable functions:

```rust
/// Assert the resource conservation invariant in a single transactional query.
///
/// INVARIANT: Σ(ALL allocated_resources) = TOTAL_AVAILABLE
async fn assert_resource_conservation(system: &ResourceManager) -> Result<()> {
    let query = sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE(SUM(allocated), 0) FROM resources"
    );
    let total: i64 = query.fetch_one(&system.pool).await?;

    if total != TOTAL_AVAILABLE {
        return Err(format!(
            "RESOURCE CONSERVATION VIOLATED: Σ(allocated) = {} (should be {})",
            total, TOTAL_AVAILABLE
        ));
    }
    Ok(())
}
```

## Common Invariant Categories

### Resource Management

- **Conservation**: `Σ(ALL allocated_resources) = TOTAL_AVAILABLE`
- **Bounded allocation**: `∀ resource r: allocated(r) <= available(r)`
- **Non-negative**: `∀ resource r: allocated(r) >= 0`
- **No over-allocation**: `∀ allocation: requested <= available`

### State Machines

- **Valid transitions**: `∀ state s: s' ∈ valid_transitions(s)`
- **Terminal states**: `∀ terminal_state: no outgoing transitions`
- **Reachability**: `∀ state s: EXISTS path from initial_state to s`
- **No cycles**: `∀ state s: s cannot reach itself through valid transitions`

### Concurrency

- **Idempotency**: `∀ operation op, ∀ N concurrent executions: at most one succeeds`
- **Atomicity**: `∀ operation: succeeds completely OR has no effect`
- **Serialization**: `∀ concurrent ops on same resource: total order exists`
- **Mutual exclusion**: `∀ resource r: at most one operation holds lock(r)`

### Data Structures

- **Queue bounds**: `∀ time t: queue.size(t) <= MAX_SIZE`
- **Ordering**: `∀ items i1, i2: IF i1.enqueued < i2.enqueued THEN i1.dequeued <= i2.dequeued`
- **Cache size**: `Σ(entry.size) <= MAX_CACHE_SIZE`
- **Tree balance**: `∀ node n: height(left(n)) - height(right(n)) ∈ {-1, 0, 1}`

### Data Integrity

- **Uniqueness**: `∀ key k: EXISTS at most one record WHERE key = k`
- **Referential integrity**: `∀ foreign_key fk: EXISTS referenced record`
- **XOR constraints**: `∀ row: (field1 > 0 AND field2 = 0) XOR (field1 = 0 AND field2 > 0)`
- **Domain constraints**: `∀ value v: v ∈ valid_domain(field)`

## Workflow

When analyzing a new subsystem:

1. **Map the public API** - List all public functions/methods
2. **For each operation** - Ask "what must be true before/after?"
3. **Trace data flow** - Follow operations through layers
4. **Review failure modes** - Check error handling, edge cases
5. **Formalize invariants** - Write in semi-formal notation
6. **Document** - Add to module documentation with Property/Enforcement structure
7. **Encode tests** - Create property tests that verify invariants
8. **Add inline comments** - Mark invariant checks in code

## Anti-Patterns to Avoid

- ❌ **Vague statements**: "The system should be consistent"
- ✅ **Specific properties**: `∀ operation_id: EXISTS at most one result WHERE id = operation_id`

- ❌ **Implementation details**: "The function uses a lock"
- ✅ **Behavioral guarantees**: `∀ concurrent executions: at most one succeeds`

- ❌ **Missing scope**: "Counts are correct"
- ✅ **Quantified scope**: `∀ resource r: allocated(r) >= 0`

## References

- See `docs/testing-handbook.md` for invariant discovery methodology
- See `lib/credit-accounting/src/tests/property.rs` for property test examples (accounting domain)
- See `lib/credit-accounting/README.md` for invariant documentation examples (accounting domain)
