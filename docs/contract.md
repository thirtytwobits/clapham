# The 0.1.0 contract

This is the release contract for Clapham 0.1.0, currently prepared as `0.1.0-rc.1`.
A change to a guarantee below must update this document and its corresponding test in the
same pull request. The linked tests are executable evidence; the detailed guides explain
configuration and usage. The [release metadata](../release.json) records the crate versions.
`cargo xtask docs` checks that evidence names exist in their linked Rust files; reviewers
still verify that those tests establish each guarantee.

Clapham is five Rust libraries. A contribution owns descriptors and handlers; the application
composes contributions, supplies policy and exposes a selection through the call runner,
CLI or MCP server. The MCP client supplies contributions for remote tools. No other London
piece is a dependency. See [composing](composing.md) for the application boundary.
Dependency evidence: [boundary tests](../tools/xtask/src/boundaries.rs),
`refuses_other_pieces_in_every_dependency_kind_even_when_renamed`.

## C01: Descriptor wire form

An `Operation` is language-neutral data. Its complete name is 1–128 ASCII letters, digits,
`_`, `-` and `.`, split into namespace and local name at the first dot. Composition requires
both parts and namespace ownership. A description is required and must not be blank. Input
declares `type: object` at its root. Output is optional and admits object or boolean JSON
Schemas; results may be any JSON shape. `idempotent` and `open_world` are required hints.
Unknown descriptor fields and malformed wire values are refused. Title, resource, positional,
preview and streaming metadata remain part of the descriptor, not handler-generated discovery.

Evidence: [contract tests](../crates/clapham/tests/contract.rs),
`descriptor_round_trips_and_validates_against_the_published_schema`,
`refuses_missing_required_fields_in_rust_and_the_schema`,
`requires_an_input_schema_with_an_explicit_object_root`,
`accepts_object_or_boolean_output_schemas_and_nullable_optional_fields` and
`refuses_malformed_and_unknown_descriptor_fields`;
[contract name tests](../crates/clapham/tests/contract.rs),
`refuses_invalid_names_at_both_wire_boundaries` and
`name_parser_and_schema_agree_on_the_whole_byte_alphabet`;
[composition tests](../crates/clapham/tests/composition.rs),
`refuses_empty_and_whitespace_descriptions_without_rewriting_valid_text` and
`output_schemas_can_describe_any_json_result_shape`.

## C02: Effects, principal defaults and hints

| Effect | Default principals | CLI confirmation | MCP read-only / destructive |
|---|---|---|---|
| `query` | human, agent, service, system | None | true / false |
| `mutation` | human, agent, service, system | None | false / false |
| `administrative` | human, service, system | `--yes`, including previews | false / false |
| `destructive` | human, service, system | `--yes`, including previews | false / true |

Omitting `principals` selects these defaults; an explicit list replaces them, and an empty
list admits nobody through `DefaultGate`. A principal retains both its identity and kind.
Queries project `idempotentHint: true`; other effects project the descriptor's `idempotent`.
`openWorldHint` follows `open_world`. Administrative tools additionally carry
`_meta["io.github.thirtytwobits.clapham/effect"] = "administrative"`.
These hints describe effects; the gate makes admission decisions. Confirmation grants no
additional permission and is a CLI safeguard, not a prerequisite for the core call API.

Evidence: [contract tests](../crates/clapham/tests/contract.rs),
`defaults_principals_from_each_effect_but_preserves_explicit_lists`,
`every_effect_maps_the_explicit_hints_and_administrative_metadata` and
`principals_keep_the_identity_and_kind_wire_form`;
[CLI execution tests](../crates/clapham-cli/tests/execution.rs),
`confirmations_and_dry_runs_are_checked_before_stdin_is_read`;
[call tests](../crates/clapham/tests/calls.rs),
`a_host_gate_can_admit_an_agent_to_an_administrative_operation`.

## C03: Composition is atomic and bounded

Composition consumes contributions and produces an immutable set of compiled operations in
name order. It does not partially register a failing contribution. Each contribution claims
one namespace; `clapham` is reserved even for empty contributions. Every set contains the
two built-ins, which consume the same budgets as contributed operations.

| Composition code | Refused condition |
|---|---|
| `namespace_claimed` | Repeated namespace or application claim to `clapham` |
| `invalid_name` | Invalid namespace, incomplete name or operation owned elsewhere |
| `missing_description` | Empty or whitespace-only description |
| `invalid_schema` | Schema compilation fails, including references requiring retrieval |
| `schema_too_large` | Input or output schema exceeds its compact UTF-8 byte budget |
| `duplicate_operation` | Complete operation name repeated within a contribution |
| `invalid_bounds` | Bounds fail supported ranges or combined buffering limit |
| `catalogue_too_large` | Compact descriptor array exceeds the catalogue budget |
| `too_many_operations` | Operation count, including built-ins, exceeds the limit |

Schemas compile offline once. A valid `Bounds` profile does not imply that a particular
catalogue fits. Connection buffers are limited in aggregate to 64 MiB; core calls themselves
do not queue or enforce the MCP connection concurrency limits.

Evidence: [composition tests](../crates/clapham/tests/composition.rs),
`composes_two_namespaces_in_name_order_and_retains_their_own_names`,
`refuses_repeated_namespaces_even_without_operations_and_reserves_its_own`,
`refuses_names_outside_the_namespace_alphabet_and_operations_owned_elsewhere`,
`refuses_invalid_input_and_output_schemas_and_unresolved_external_references`,
`checks_each_schema_size_at_the_exact_serialised_byte_boundary` and
`refuses_duplicate_operations_and_catalogue_count_or_byte_overflow`;
[call type tests](../crates/clapham/tests/call_types.rs),
`invalid_bounds_are_refused_before_composition_including_overflow_and_total_buffering`;
[built-in tests](../crates/clapham/tests/builtins.rs),
`built_in_operations_and_schemas_consume_the_same_finite_composition_budgets`.

## C04: Selection and revision identify contract data

A selection is a sorted, deduplicated subset of complete names. Unknown names refuse.
Selections belong to their originating composed set and its clones; an independent set
cannot use one even if its descriptors and revision match. Revisions are canonical SHA-256
digests, stable across descriptor construction order and processes. Handlers, bounds,
principals and gates do not enter the digest. Selected remote binding revisions do.
A revision is neither permission to dispatch nor evidence of an effect.

Evidence: [call tests](../crates/clapham/tests/calls.rs),
`selection_lists_a_sorted_deduplicated_subset_and_refuses_unknown_names`,
`an_unselected_or_foreign_selection_never_runs_a_handler_or_gate`,
`revision_selection_accounts_only_for_selected_descriptors_and_bindings` and
`revision_bound_selection_matches_an_independent_golden_digest`;
[composition tests](../crates/clapham/tests/composition.rs),
`revision_is_independent_of_contribution_and_nested_object_key_order`,
`revision_excludes_handlers_bounds_and_operation_construction_order` and
`revision_is_stable_across_processes`. [Calls](calls.md#revisions-for-approval) records framing.

## C05: Call order and gate

`ComposedSet::call` returns `CallResult`: an `Outcome` or typed `Refusal`. Cancellation already
signalled on entry precedes every other check. Otherwise the order is selection membership,
input validation, gate, preview support, locality, cancellation again, handler and successful
output validation. The handler receives the caller, its own operation name, input, preview,
record sink and cancellation signal. Synchronous validation and gate checks are checkpoints,
not pre-emptible work.

`Allow` admits; `Deny` produces `not_admitted`; `Pending` produces retryable
`admission_pending`. Neither denied nor pending calls dispatch. Admission is rechecked on
each call, including previews. In-process mode refuses daemon-local operations as
`no_daemon`; daemon mode admits either locality subject to the other checks.
The low-level `ComposedOperation::invoke` validates and invokes but does not enforce the
selection, gate, preview, locality or declared-refusal policy. User calls use the set runner.

Evidence: [call tests](../crates/clapham/tests/calls.rs),
`pre_cancelled_calls_skip_validation_and_admission_without_dispatch`,
`input_validation_precedes_the_gate_and_prevents_dispatch`,
`pending_and_denied_admission_do_not_dispatch_and_precede_preview_checks`,
`gate_decisions_are_consulted_on_every_call`,
`preview_requires_support_and_is_passed_unchanged_to_the_handler` and
`daemon_locality_refuses_in_process_but_runs_on_a_daemon`;
[composition tests](../crates/clapham/tests/composition.rs),
`boxed_handlers_can_borrow_state_and_receive_validated_input_under_the_own_name`.

## C06: Refusals retain contributor codes

| Category | CLI exit code |
|---|---|
| `usage` | 2 |
| `invalid_input` | 65 |
| `not_found` | 66 |
| `conflict` | 67 |
| `refused` | 77 |
| `unavailable` | 69 |
| `interrupted` | 75 |
| `configuration` | 78 |
| `internal` | 70 |

A declared handler refusal keeps code, message, details, retryability and evidence; its
category comes from the descriptor. An undeclared code becomes non-retryable
`undeclared_refusal` in `internal`, retaining the original refusal in its details and its
evidence on the wrapper. Runner-generated refusals do not require declarations. A handler
code resembling a runner or transport code still follows the declared-refusal rule.
CLI refusals are one unchanged compact JSON line on stderr, independent of output mode.

Evidence: [contract tests](../crates/clapham/tests/contract.rs),
`refusal_categories_have_the_declared_wire_spelling_and_exit_codes` and
`refusals_round_trip_without_changing_contributor_codes`;
[call tests](../crates/clapham/tests/calls.rs),
`declared_refusals_keep_the_code_and_use_the_descriptors_category` and
`undeclared_handler_refusals_preserve_the_original_including_reserved_looking_codes`;
[process tests](../crates/clapham-conformance/tests/cli_process.rs),
`every_refusal_category_uses_one_json_stderr_line_and_its_exit_code`.

## C07: Evidence records dispatch knowledge

`not_dispatched` means the handler was not entered; `completed` means a response completed;
`uncertain` leaves effects unresolved. Pre-dispatch refusals use `not_dispatched`. In-flight
cancellation drops the handler future and returns retryable `cancelled` with `uncertain`
evidence. Cancellation wins if completion is ready at the same poll. Returned outcomes and
refusals retain their explicit evidence, including output-validation failures. Completed
evidence does not promise atomic effects, rollback or durable publication.

Evidence: [call type tests](../crates/clapham/tests/call_types.rs),
`every_refusal_evidence_state_round_trips_and_validates_against_the_published_schema`;
[composition tests](../crates/clapham/tests/composition.rs),
`cancelling_a_scripted_in_flight_handler_yields_uncertain_evidence`,
`cancellation_wins_when_the_held_handler_is_also_ready_to_complete` and
`handler_refusals_and_uncertain_remote_evidence_pass_through_unchanged`;
[call tests](../crates/clapham/tests/calls.rs), `invalid_output_is_an_internal_completed_refusal`.

## C08: Outcomes and records preserve their meaning

Absent structured output differs from JSON null; absent content differs from an empty list.
Ordered text, image, audio, resource-link and embedded-resource blocks are retained. A
tool-reported error is an `Outcome` with `is_error`, not a dispatch refusal; its structured
error result is not validated as successful output. Image/resource references are not fetched
by the core.

Handlers await each asynchronous sink acknowledgement to maintain backpressure and order.
The CLI delivers records before one terminal result; both use `{v: 1, kind, data}` envelopes
in JSON modes. MCP 0.1.0 drains records to a discard sink and returns only the final result.
A propagated sink refusal follows the handler's declared-refusal contract.

Evidence: [call type tests](../crates/clapham/tests/call_types.rs),
`ordered_content_keeps_all_block_kinds_and_binary_payloads_without_fetching` and
`absent_and_null_structured_results_and_absent_and_empty_content_remain_distinct`;
[call tests](../crates/clapham/tests/calls.rs),
`tool_reported_errors_remain_outcomes_with_their_own_result_shape` and
`streaming_records_arrive_in_order_before_the_result_with_backpressure`;
[process tests](../crates/clapham-conformance/tests/cli_process.rs),
`piped_export_flushes_ordered_frames_then_one_result`;
[MCP stdio tests](../crates/clapham-conformance/tests/mcp_stdio.rs),
`stdio_lists_effects_calls_refusals_resources_and_preview`;
[fixture tests](../crates/clapham-conformance/tests/contribution.rs),
`an_empty_export_returns_zero_and_a_sink_refusal_keeps_its_code_and_evidence`.

## C09: Discovery applies the caller's selection and gate

Every set supplies `clapham.describe` and `clapham.version`. Describe lists selected,
gate-allowed descriptors in name order with schemas. Denied and pending candidates are
hidden. A named lookup treats unknown, unselected, denied and pending names identically as
`unknown_operation`. The gate is consulted again on each request. The outer built-in call
also requires selection and admission. Discovery is unpaged and bounded by message bytes.
Version returns the core package version and the application identity supplied at composition.

Evidence: [built-in tests](../crates/clapham/tests/builtins.rs),
`an_empty_host_gets_both_built_ins_in_the_registry_and_all_selection`,
`named_discovery_returns_the_exact_descriptor_including_its_schemas`,
`selection_membership_and_admission_hide_names_with_the_same_unknown_refusal`,
`discovery_rechecks_changing_gate_state_on_each_request`,
`the_outer_gate_can_deny_or_defer_discovery_before_any_candidate_is_examined`,
`discovery_accepts_the_exact_utf8_result_budget_and_refuses_one_byte_less` and
`version_reports_the_core_and_exact_application_metadata_from_composition`.

## C10: Front ends preserve the call boundary

The CLI derives nested commands and typed input binding from descriptors. Whole-object JSON
and property arguments cannot be mixed. Syntax failures are `usage`; a bound document that
fails its schema retains `invalid_input`. Confirmation and unsupported-preview parsing
checks precede stdin reads. Terminal output defaults to human fields/tables; pipes default
to NDJSON envelopes. CLI 0.1.0 executes workspace operations only, even if given a daemon-mode
set; it does not route daemon operations over MCP.

The MCP server retains descriptor names, hints and schemas. Refusals become tool errors with
the unchanged refusal in `structuredContent`. Stdio identity comes from the launcher; HTTP
authenticates each request before body consumption, gate or handler. Metadata cannot replace
that identity. Discovery, calls and resources share the selection and gate. Byte and request
budgets are finite; an oversized refusal is never replaced with a different refusal code.

Evidence: [CLI tree tests](../crates/clapham-cli/tests/tree.rs),
`json_cannot_mix_with_positionals_and_unknown_arguments_are_usage`,
`schema_refusals_are_carried_unchanged_for_flags_json_and_prepared_calls` and
`daemon_operations_never_run_even_in_a_daemon_composed_set`;
[process tests](../crates/clapham-conformance/tests/cli_process.rs),
`terminal_defaults_to_human_and_explicit_json_is_pretty` and
`confirmation_and_preview_are_enforced_before_effects`;
[MCP tests](../crates/clapham-conformance/tests/mcp.rs),
`selection_limits_tools_resources_and_direct_calls_without_exposing_describe`,
`content_null_and_boolean_output_schema_retain_their_meaning` and
`oversized_refusal_closes_instead_of_changing_its_code`;
[HTTP tests](../crates/clapham-conformance/tests/mcp_http.rs),
`unmapped_credentials_are_refused_before_body_or_gate_or_handler`,
`concurrent_requests_keep_their_own_authenticated_principals` and
`http_request_and_reply_bytes_are_bounded`.

## C11: Remote bindings do not grant authority or replay effects

The client maps remote tools to daemon-local contributions. The host supplies credential
scope, aliases, principal authority and finite connection limits. Remote metadata cannot
grant access. Bindings pin remote definitions; tool-list notifications and failed refreshes
invalidate them. Binding and selection revisions include credential scope and exposed data.
Lost replies and invocation deadlines are uncertain; reconnecting never replays a call.
A remote Clapham refusal remains a tool-error `Outcome`, with its original structured code.
The [client contract](mcp.md#client) gives effect mapping and adapter refusal codes.

Evidence: [client contract tests](../crates/clapham-mcp-client/tests/contracts.rs),
`effect_claims_and_principal_authority_remain_separate`,
`notifications_and_refresh_guard_pinned_definitions`,
`binding_and_selection_revisions_fence_metadata_and_credentials`,
`applied_effect_with_lost_reply_is_uncertain_and_never_replayed` and
`discovery_bounds_and_failed_refresh_invalidate_existing_bindings`;
[client/server conformance tests](../crates/clapham-conformance/tests/mcp_client.rs),
`stdio_binding_calls_the_same_lists_contribution` and
`http_binding_authenticates_and_calls_the_same_lists_contribution`.

## C12: One fixture can be checked through every front end

`clapham_conformance::run` compares explicit finite cases from fresh fixture state against an
independent in-process baseline. It checks results, refusal fields, CLI exit codes, gate
visibility and ordered records on record-bearing front ends. It rejects namespace collisions
before starting a driver. Raw MCP drivers compare describe and every tools/list page;
client-bound drivers require native `mcp_selection` for tools hidden before binding.
The reference suite launches real CLI, stdio and HTTP processes and client bindings to both.

Evidence: [harness tests](../crates/clapham-conformance/tests/harness.rs),
`dropping_retryable_fails_the_harness`, `changed_refusal_codes_and_categories_fail`,
`wrong_records_early_results_and_duplicate_results_fail`,
`wrong_exit_code_and_gate_visibility_fail` and
`namespace_collision_and_missing_cases_fail_before_a_driver_starts`;
[process driver tests](../crates/clapham-conformance/tests/harness_process.rs),
`tools_list_leaks_and_cross_page_duplicates_fail_even_when_describe_is_correct`;
[six-front-end matrix](../examples/lists/tests/conformance.rs),
`lists_passes_every_front_end_as_real_processes`.

## Host responsibilities

The application owns workspace locks, persistence, transactions, credentials, listeners,
process reaping, shutdown and any approval workflow. A gate must be safe to query repeatedly
for visibility; it must not start or consume approvals during discovery. The host decides
what to do after `admission_pending` and how to reconcile uncertain effects. Effect hints and
revisions do not prove application correctness or durable effects. No daemon discovery,
automatic CLI-to-daemon routing or durable approval coordinator is supplied in 0.1.0.
