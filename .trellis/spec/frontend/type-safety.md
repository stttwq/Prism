# Type Safety

## Scenario: Typed Targets And Versioned Settings

### 1. Scope / Trigger

Applies to Rust/WPF search results and persisted settings.

### 2. Signatures

- `ActionTarget(string Kind, string Value)`.
- `SearchResult.Target` is optional; `ExecutionTarget` owns legacy conversion.
- Settings schema is 1; missing legacy version is 0.
- `ExcludedPaths` contains at most 32 absolute paths.

### 3. Contracts

- Wire names are lowercase `kind` and `value`.
- Unknown result kinds stay `Unknown`; broker rejects unknown target kinds.
- Readers default missing fields, normalize null collections, and reject future schemas.
- Writers validate before atomic replacement.

### 4. Validation & Error Matrix

| Condition | Result |
| --- | --- |
| Missing target | One legacy conversion |
| Unknown target | Broker `unsupported` |
| >32 exclusions | `InvalidDataException` |
| Relative/control/oversized exclusion | `InvalidDataException` |
| Future schema | Safe defaults |

### 5. Good / Base / Bad Cases

- Good: `{kind:"file",value:"C:\\..."}` end to end.
- Base: old result falls back through `ExecutionTarget`.
- Bad: commands independently guess from `ExecuteId`.

### 6. Tests Required

- Old optional shape, unknown result kind, typed target precedence.
- Missing/null/future settings and validated writes.
- WPF Release build and C# tests.

### 7. Wrong vs Correct

```csharp
// Wrong: PascalCase wire fields
new { type = "execute", target };
// Correct
new { type = "execute", target = new { kind = target.Kind, value = target.Value } };
```
