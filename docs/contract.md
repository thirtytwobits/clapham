# Operation contract

Clapham defines transport-independent operations. Applications own OAuth grant issuance,
account and resource policy, persistence, workspace ownership and lifecycle. The core has
no dependency on MCP or another application. Changes to these guarantees update their test
evidence in the same change. `cargo xtask docs` checks evidence references and quickstart drift.

Dependency evidence: [boundary tests](../tools/xtask/src/boundaries.rs),
`refuses_other_pieces_in_every_dependency_kind_even_when_renamed`.

## C01: Typed definitions and descriptor wire form

Typed handlers derive input schemas for deserialisation and output schemas for serialisation.
Wire conversion occurs inside the dispatcher. Names, descriptions, object-root input schemas,
optional output schemas, effects, required scopes, policy names, preview, records and CLI
settings remain transport-independent descriptor data. Unknown descriptor fields are refused.

Evidence: [descriptor tests](../crates/clapham/tests/contract.rs),
`descriptor_round_trips_and_validates_against_the_published_schema`,
`requires_an_input_schema_with_an_explicit_object_root` and
`refuses_malformed_and_unknown_descriptor_fields`;
[typed tests](../crates/clapham/tests/configuration.rs),
`typed_authoring_preserves_schema_constraints_refusals_and_conversion_failures`,
`typed_schemas_preserve_nested_references_renaming_and_unknown_field_rules`,
`typed_conversion_failures_preserve_dispatch_evidence_and_output_validation` and
`typed_outcomes_preserve_content_error_status_and_evidence`.

## C02: OAuth authority and permissions

Trusted middleware supplies verified issuer, audience, subject, client, scopes and extension
claims. Configured issuer/audience bindings and every required scope are checked by the shared
runner. Host admission can restrict a grant but cannot grant missing scopes. Effects describe
behaviour; confirmation grants no authority. Resource policies receive verified context and
typed input before handler dispatch, including grant-specific resource restrictions.

HTTP introspection is bounded to 64 KiB and uses the OAuth library and validates activity, issuer, audience, subject,
expiry and not-before. A stdio host binds verified authority to its private connection.
Request arguments and metadata cannot replace authority. Scope elevation is controlled by
the authorisation server.

Evidence: [core permission tests](../crates/clapham/tests/configuration.rs),
`equivalent_grants_have_equivalent_access_and_resource_binding_is_checked` and
`composition_aliases_preserve_intrinsic_requirements_and_original_handler_names` and
`policies_accept_schema_annotations_but_reject_validation_and_default_changes`;
[introspection tests](../crates/clapham-mcp-server/src/oauth.rs),
`introspected_authority_checks_activity_issuer_audience_and_validity`;
[HTTP/CLI integration](../crates/clapham-conformance/tests/oauth_operations.rs),
`verified_grants_scopes_resource_policies_and_cli_records_share_one_runner`,
`native_http_records_wait_for_client_acknowledgement_and_disconnect_cancels_dispatch` and
`native_http_client_rejects_missing_duplicate_truncated_and_oversized_terminals` and
`oversized_terminal_outcomes_report_the_size_error_without_rewriting_domain_refusals`.

## C03: One atomic configuration path

JSON and typed Rust configuration use one model and composition/semantic validation path.
JSON can be loaded, supplied with executable registrations and extended programmatically.
Equivalent definitions and restrictions produce equivalent sets and revisions. Configuration
preserves intrinsic provider requirements and adds restrictions. Unknown registrations,
duplicate sources/names, invalid aliases/schemas, incompatible policies and exhausted budgets
refuse composition. Schema compilation is offline. Each namespace has one owner.

Evidence: [configuration tests](../crates/clapham/tests/configuration.rs),
`json_rust_and_mixed_configuration_share_definitions_permissions_and_revisions`,
`json_rust_and_mixed_configuration_reject_the_same_semantic_errors` and
`composition_aliases_preserve_intrinsic_requirements_and_original_handler_names` and
`policies_accept_schema_annotations_but_reject_validation_and_default_changes`;
[composition tests](../crates/clapham/tests/composition.rs),
`refuses_repeated_namespaces_even_without_operations_and_reserves_its_own`,
`refuses_invalid_input_and_output_schemas_and_unresolved_external_references` and
`checks_each_schema_size_at_the_exact_serialised_byte_boundary`.

## C04: Selection and revisions identify definitions

Selections are sorted, deduplicated subsets bound to their originating set and its clones.
Canonical SHA-256 revisions depend on descriptors and relevant remote bindings, independently
of construction order. Revisions and selections do not grant authority. Scope and policy
requirements participate in descriptor revisions; handler implementations and host policy do not.

Evidence: [call tests](../crates/clapham/tests/calls.rs),
`selection_lists_a_sorted_deduplicated_subset_and_refuses_unknown_names`,
`an_unselected_or_foreign_selection_never_runs_a_handler_or_gate`,
`revision_selection_accounts_only_for_selected_descriptors_and_bindings` and
`revision_bound_selection_matches_an_independent_golden_digest`;
[composition tests](../crates/clapham/tests/composition.rs),
`revision_is_independent_of_contribution_and_nested_object_key_order`.

## C05: Shared execution boundary

Pre-cancelled calls stop first. The runner otherwise checks selection, OAuth resource binding,
scopes, input/schema conversion, host admission, preview, locality and typed resource policies
before dispatch. Resource-policy refusals use the operation's declared codes and categories,
with not-dispatched evidence. Policy input-conversion errors remain dispatcher errors.
Successful output is schema-validated. Pending or denied admission never runs
the handler. Resource policies do not run during discovery because resource input is absent.
Trusted low-level invocation remains a dispatch primitive; external adapters use the set runner.
Plain composition may omit the OAuth resource binding for hosts that establish issuer and
audience elsewhere. HTTP servers require an explicit binding, supplied by `Configuration` or
`ComposeOptions::oauth`.

Evidence: [call tests](../crates/clapham/tests/calls.rs),
`pre_cancelled_calls_skip_validation_and_admission_without_dispatch`,
`input_validation_precedes_the_gate_and_prevents_dispatch`,
`pending_and_denied_admission_do_not_dispatch_and_precede_preview_checks` and
`preview_requires_support_and_is_passed_unchanged_to_the_handler`;
[configuration tests](../crates/clapham/tests/configuration.rs),
`json_rust_and_mixed_configuration_share_definitions_permissions_and_revisions` and
`resource_policy_cancellation_stops_before_handler_dispatch`,
`resource_policy_refusals_obey_declarations_and_preserve_not_dispatched_evidence` and
`policy_input_conversion_failure_remains_a_dispatcher_error`;
[HTTP authority tests](../crates/clapham-conformance/tests/mcp_http.rs),
`http_requires_an_explicit_oauth_resource_binding` and
`mismatched_verified_authority_is_rejected_by_both_http_interfaces`.

## C06: Refusals retain contributor codes

Declared handler and resource-policy refusals retain code, message, details, retryability and evidence. Their
category comes from the descriptor. An undeclared code becomes `undeclared_refusal`, retaining
the original refusal and its evidence. Dispatcher refusals do not require declarations, and a
record-sink refusal the handler returns is carried unchanged without declaration.
Categories map to CLI exits: usage 2, invalid input 65, not found 66, conflict 67, refused 77,
unavailable 69, interrupted 75, configuration 78 and internal 70.

Evidence: [call tests](../crates/clapham/tests/calls.rs),
`declared_refusals_keep_the_code_and_use_the_descriptors_category` and
`undeclared_handler_refusals_preserve_the_original_including_reserved_looking_codes`,
`a_returned_sink_refusal_is_carried_unchanged_without_declaration` and
`a_handler_refusal_other_than_the_sinks_still_requires_declaration`;
[process tests](../crates/clapham-conformance/tests/cli_process.rs),
`every_refusal_category_uses_one_json_stderr_line_and_its_exit_code`.

Evidence: [typed refusal tests](../crates/clapham/tests/configuration.rs),
`typed_handlers_cannot_pass_domain_refusals_off_as_conversion_failures`.

## C07: Evidence records dispatch knowledge

`not_dispatched` means the handler was not entered; `completed` means a response completed;
`uncertain` leaves effects unresolved. In-flight cancellation drops the handler future and
returns uncertain evidence. Cancellation wins if completion is ready at the same poll.
Explicit handler evidence survives output-validation failures. Completed does not imply
atomic effects, rollback or durable publication.

Evidence: [composition tests](../crates/clapham/tests/composition.rs),
`cancelling_a_scripted_in_flight_handler_yields_uncertain_evidence`,
`cancellation_wins_when_the_held_handler_is_also_ready_to_complete` and
`handler_refusals_and_uncertain_remote_evidence_pass_through_unchanged`;
[call tests](../crates/clapham/tests/calls.rs),
`invalid_output_is_an_internal_completed_refusal`.

## C08: Outcomes and ordered records

Absent structured output differs from JSON null; absent content differs from an empty list.
Adapters own native content types and carry opaque content through the core. Tool-reported
errors remain outcomes and do not undergo successful-output validation. Records are awaited
in order before one terminal result. Operation HTTP uses bounded NDJSON frames and acknowledges
record delivery through one-use tickets bound to verified OAuth authority. Producers wait
for the client sink to accept and acknowledge each record. Disconnect cancels work. MCP
carries records under the records extension: one notification per record on the call's
response stream, acknowledged by a request bound to the grant that began the call.
Acknowledgements are admitted outside the request limits; repeating the last is accepted, and
the client cancels on one it cannot confirm. The record sink refuses a notification over
the message bound with `result_too_large` and an unacknowledged record with
`record_delivery_failed`, both with uncertain evidence, and a handler that returns the
refusal carries it unchanged (C06). A client without the extension receives the final result
alone. If an outcome fits the core bound but its stream envelope exceeds it, HTTP sends `result_too_large` with uncertain evidence. An oversized
existing domain refusal closes the stream.

Evidence: [call type tests](../crates/clapham/tests/call_types.rs),
`ordered_content_keeps_all_block_kinds_and_binary_payloads_without_fetching` and
`absent_and_null_structured_results_and_absent_and_empty_content_remain_distinct`;
[call tests](../crates/clapham/tests/calls.rs),
`streaming_records_arrive_in_order_before_the_result_with_backpressure`;
[HTTP/CLI integration](../crates/clapham-conformance/tests/oauth_operations.rs),
`verified_grants_scopes_resource_policies_and_cli_records_share_one_runner`,
`native_http_records_wait_for_client_acknowledgement_and_disconnect_cancels_dispatch` and
`native_http_client_rejects_missing_duplicate_truncated_and_oversized_terminals` and
`oversized_terminal_outcomes_report_the_size_error_without_rewriting_domain_refusals`;
[MCP records tests](../crates/clapham-conformance/tests/mcp_records.rs),
`records_arrive_in_order_each_acknowledged_before_the_result_over_stdio`,
`records_stream_before_the_result_while_the_call_holds_every_permit_over_http`,
`a_client_without_the_extension_receives_the_result_alone_over_stdio`,
`acknowledgements_are_idempotent_and_bound_to_the_record_sent_over_stdio`,
`acknowledgements_from_another_grant_are_refused_over_http`,
`the_acknowledgement_path_answers_nothing_else_over_http`,
`an_oversized_record_refuses_the_call_with_uncertain_evidence`,
`cancelling_a_call_awaiting_acknowledgement_drops_its_handler_over_stdio` and
`closing_the_stream_cancels_a_call_awaiting_acknowledgement_over_http`;
[client records tests](../crates/clapham-mcp-client/tests/records.rs),
`a_streaming_tool_passes_records_to_the_sink_and_acknowledges_each_after_it`,
`a_result_completes_the_call_while_its_acknowledgement_reply_is_outstanding`,
`a_refused_acknowledgement_cancels_the_call_with_uncertain_evidence`,
`a_sink_refusal_cancels_the_call_without_acknowledging`,
`a_record_out_of_sequence_cancels_the_call` and
`a_lost_acknowledgement_reply_is_repeated_over_http`;
[rmcp server facts](../crates/clapham-mcp-server/src/rmcp_facts.rs),
`held_call_notifications_precede_its_result_in_order_over_stdio`,
`held_call_notifications_precede_its_result_in_order_over_stateless_http`,
`custom_requests_reach_the_handler_while_a_call_is_held_over_stdio` and
`custom_requests_reach_the_handler_while_a_call_is_held_over_stateless_http`;
[rmcp client facts](../crates/clapham-mcp-client/tests/rmcp_facts.rs),
`custom_notifications_during_a_call_reach_the_client_handler` and
`custom_notification_handlers_are_not_ordered_with_one_another`.

## C09: Discovery respects authority

Describe and MCP discovery list selected, scope-admitted and host-admitted definitions.
Named discovery hides unknown, unselected and denied names consistently. Invocation rechecks
permissions and resource access. Built-in discovery and version consume the same budgets as
contributed operations. Discovery is bounded, and resource policies are deferred until calls.

Evidence: [built-in tests](../crates/clapham/tests/builtins.rs),
`selection_membership_and_admission_hide_names_with_the_same_unknown_refusal`,
`discovery_rechecks_changing_gate_state_on_each_request`,
`discovery_accepts_the_exact_utf8_result_budget_and_refuses_one_byte_less` and
`version_reports_the_core_and_exact_application_metadata_from_composition`.

## C10: Adapters preserve the call boundary

The server-backed CLI derives commands and input binding from admitted descriptors. It sends
operation arguments and preview, not authority claims. Whole-object JSON and property arguments
cannot be mixed. Confirmation and unsupported-preview parsing precede stdin reads. Human output
uses fields/tables; pipes use versioned NDJSON. HTTP disables redirects and retries. Trusted
in-process CLI embedding requires host-verified authority.

MCP tools/resources use rmcp and the shared runner. HTTP authenticates each request before
body consumption; concurrent requests keep separate contexts. Byte/request budgets remain
finite. Oversized existing refusals are not replaced by different domain codes.

Evidence: [CLI tests](../crates/clapham-cli/tests/tree.rs),
`json_cannot_mix_with_positionals_and_unknown_arguments_are_usage`;
[execution tests](../crates/clapham-cli/tests/execution.rs),
`confirmations_and_dry_runs_are_checked_before_stdin_is_read`;
[HTTP tests](../crates/clapham-conformance/tests/mcp_http.rs),
`unmapped_credentials_are_refused_before_body_or_gate_or_handler` and
`concurrent_requests_keep_their_own_authenticated_auths`;
[MCP tests](../crates/clapham-conformance/tests/mcp.rs),
`oversized_refusal_closes_instead_of_changing_its_code`.

## C11: Remote bindings fence definitions and effects

The host establishes audience-appropriate upstream credentials and explicit local scope
requirements. Remote hints cannot grant permissions. Bindings pin selected definitions and
credential scope. Notifications and failed refreshes invalidate stale bindings. Lost replies
and invocation deadlines are uncertain. Reconnecting does not replay effects. Remote domain
refusals remain structured tool-error outcomes with their original codes.

Evidence: [client tests](../crates/clapham-mcp-client/tests/contracts.rs),
`effects_describe_behaviour_without_granting_or_removing_oauth_scopes`,
`notifications_and_refresh_guard_pinned_definitions`,
`binding_and_selection_revisions_fence_metadata_and_credentials`,
`applied_effect_with_lost_reply_is_uncertain_and_never_replayed` and
`discovery_bounds_and_failed_refresh_invalidate_existing_bindings`.

## C12: Cross-adapter conformance

Fresh fixtures compare results, refusals, exit codes, visibility and ordered records.
The reference executable runs all six front ends, including the HTTP-backed CLI. An operation
hidden before CLI parsing fails as `usage`; one hidden before remote binding fails as
`mcp_selection`. Visible-operation domain refusal fields are compared unchanged.

Evidence: [harness tests](../crates/clapham-conformance/tests/harness.rs),
`dropping_retryable_fails_the_harness`, `changed_refusal_codes_and_categories_fail`,
`wrong_records_early_results_and_duplicate_results_fail` and
`namespace_collision_and_missing_cases_fail_before_a_driver_starts`;
[process matrix](../examples/lists/tests/conformance.rs),
`lists_passes_every_front_end_as_real_processes`.
