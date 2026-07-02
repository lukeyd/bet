# `tests/mir` — hand-written `.mir` files

The textual serialization of the mid-level IR (owned by `crates/midir`). Lets the backend
team start immediately from hand-written IR test cases, before the frontend emits any
(bootstrap-plan.md §1a). Populated in **Step 1a**. Not a Cargo crate — these are `.mir`
text consumed by `midir`'s parser and the backend.
