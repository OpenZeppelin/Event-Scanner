## Input Validation Plan for Event Scanner

**Status**: DRAFT - To be deleted once agreed upon and implemented

This document outlines the input validation rules we plan to add to ensure the library handles edge cases

---

### 1. **Count Parameter Validation**

**Applies to:**
- `EventScannerBuilder::latest(count)` 
- `EventScannerBuilder::sync().from_latest(count)`

**Validation:**
- `count` must be greater than 0
- **Rationale:** A count of 0 doesn't make sense

**Implementation approach:**
- Use `assert!(count > 0, "count must be greater than 0")` in the constructor methods


### 2. **Max Block Range Validation**

**Applies to:**
- `max_block_range(n)` 
- Also exposed on all scanners

**Validation:**
- Set a minimum of 1 block
- **Rationale:** Can't scan less than one block

**Implementation approach:**

### Question

Should we add info! tracing on 'abnormal' values i.e >64 block confirmations 
