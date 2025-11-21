# YAML Validation Test Cases

Test procedure files with various errors to verify Monaco editor error display.
Each file corresponds to ONE validation step in the backend pipeline.

## How to Test

1. Open Studio
2. Open any test file from this directory as a procedure
3. Verify error markers appear in Monaco editor
4. Check error message on hover

## Validation Pipeline Steps

### 1. YAML Syntax Validation (serde_yaml parsing)
**File:** `01-yaml-syntax-errors.yaml`
- Invalid indentation
- Unclosed brackets
- Missing colons
- Duplicate keys at same level
- Invalid tab characters
- Unclosed quotes
- Invalid list items

### 2. Schema Validation (serde deserialization)
**File:** `02-schema-validation-errors.yaml`
- Missing required fields
- Wrong types (string vs array, number vs string)
- Invalid enum values
- Invalid nested structures
- Invalid boolean values
- Unknown field names

### 3. Structure Validation (duplicates, dependencies)
**File:** `03-structure-validation-errors.yaml`
- Duplicate phase keys
- Duplicate plug keys
- Duplicate measurement keys (within same phase)
- Orphaned dependencies (nonexistent phases)
- Self-dependencies
- Circular dependencies (2, 3, 4 phases)

### 4. Python Module Validation
**File:** `04-python-validation-errors.yaml`
- Missing Python modules in main phases
- Missing Python modules in setup phases
- Missing Python modules in teardown phases
- Missing Python modules in plugs

## Expected Behavior

Each error file should show:
1. Multiple red squiggly underlines at different error locations
2. Error messages on hover for each error
3. Correct line and column numbers for each diagnostic
4. Error severity indicators
5. All errors from that validation step displayed simultaneously
