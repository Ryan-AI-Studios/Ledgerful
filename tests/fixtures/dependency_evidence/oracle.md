# Dependency evidence fixture oracle

The source cases are hand-authored and use ordinary source constructs. Exact native report expectations below derive from the extractors and basename resolver as traced before implementation. Remaining outcomes require fixture replay.

| Case | Source location | Source relation | Expected native report | Expected frozen Rust SCIP addition |
|---|---|---|---|---|
| rust-direct | main.rs:2 | rust_caller calls main.rs rust_target | resolved nativeCall with those symbols | Same pair ends with scipReference |
| ts-direct | main.ts:2 | tsCaller calls main.ts tsTarget | resolved nativeCall with those symbols | none |
| python-direct | main.py:5 | py_caller calls main.py py_target | resolved nativeCall with those symbols | none |
| rust-import | main.rs:4 | rust_alias binds rust_defs.rs real_rust | no call witness | none |
| rust-alias-call | main.rs:5 | alias_rust_caller calls rust_defs.rs real_rust | unresolved nativeCall, null callee | none |
| ts-import | main.ts:3 | tsAlias binds ts_defs.ts realTs | no call witness | none |
| ts-alias-call | main.ts:4 | aliasTsCaller calls ts_defs.ts realTs | unresolved nativeCall, null callee | none |
| ts-re-export | ts_re_export.ts:1 | ts_defs.ts realTs exported as reExportedTs | no call witness | none |
| python-import | main.py:1 | py_alias binds python_defs.py real_py | no call witness | none |
| python-alias-call | main.py:7 | alias_py_caller calls python_defs.py real_py | unresolved nativeCall, null callee | none |
| runtime-bridge-mention | bridge.ts:1 | comment names a possible runtime bridge | no call witness or cross-language resolution | none |

Every non-resolved case forbids a concrete callee witness. In particular a bare alias must never be credited to a similarly named but different target. The only engineered SCIP relation is the Rust direct pair. The fixture records no producer behavior outside the pinned artifact and certifies no import or cross-language coverage.

## Frozen artifact

`fixture.scip` contains one classic-range document for `main.rs`. Line coordinates in the wire payload are zero-based. The two definitions are at (0, 3–14) and (1, 7–18). One reference at (1, 23–34) targets the first definition and replaces its native overlap. A second reference at the same span names an absent definition and increments `edges_skipped_unmapped`. A two-integer range increments `edges_skipped_invalid_occ_range`. The expected totals are two definitions seen and mapped, three references seen, one existing edge updated and zero edges added. `malformed.scip` is a truncated length-delimited field and must fail ingest while leaving native indexing available.

The manifest pins SHA-256 of each input. These bytes were authored from the SCIP protocol fields alone. No Ledgerful resolver or external indexer produced the oracle or payload. The simulated executable cases exercise process selection and native-floor behavior independently.
