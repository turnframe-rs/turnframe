# Release checklist and production-readiness gates

Turnframe is not production-ready, and no version is tagged as stable, until every gate below is
checked with the evidence named next to it. The gates are the ones defined by the master
specification (§33); this document adds, for each gate, what counts as proof and where it lives.

Evidence categories used throughout:

- **Test**: a test that exists and passes in CI, named exactly as it is written in the source, with
  the file it lives in. A gate is not satisfied by a test that merely exercises the area, it needs a
  test whose assertion is the gate itself.
- **Check**: a named conformance function or `Check` row that every implementation of a contract
  (store, executor, provider adapter) is run against, rather than a test of one implementation.
- **Invariant**: a violation the state explorer or the core invariant checker can report, together
  with the test that proves it is reported.
- **Doc**: a document in `docs/` (or an ADR under `docs/adr/`) that states the rule and its
  rationale.
- **Metric**: a `turnframe.*` counter that makes the failure mode visible in production. A metric
  is only evidence if something in the pipeline emits it; where a metric is declared in
  `turnframe-core`'s `observe` module but no code path emits it, that is said in as many words.

## Where this workspace stands

Of the 33 gates below, **all 33 are met**.

The evaluation gate is met by a corpus that runs against real endpoints, not only a scripted
provider: a scripted run measures the fixture, since its assertions pass because the script was
written to make them pass. The live corpus holds 76 items in English and Italian, each checked by
deterministic assertions and, where it says so, task by task. The latest figures and their limits
are in [benchmarks](benchmarks.md): on OpenAI's `gpt-5.4-mini` at `medium`, 228 of 228 samples
pass, three samples per item. Re-run it as [evaluation](evaluation.md) describes.

Two things the live runs taught are now part of the contract. A schema is rewritten into each
provider's dialect, narrowing only and refusing what it cannot carry, and each adapter has a live
test that puts a real schema in front of the real endpoint. And a model is never asked to count
characters: it points at numbered words, and a text value also copies them.

Three obligations attached to otherwise-met gates genuinely cannot be closed before a first release,
and are called out where they occur rather than left to look like failures:

- the live smoke-test summary that the adapter-conformance gate wants recorded in the release notes,
  which needs release notes to exist;
- the `cargo-semver-checks` baseline in the mechanical steps, which needs a previously published
  version to compare against;
- any reliability figure, which by the rule at the foot of this page comes from observed production
  metrics and from nowhere else.

## Safety gates

- [x] **No consequential command can originate from raw model output.**
  Test: `a_consequential_sample_command_needs_a_trusted_origin`,
  `a_trusted_origin_is_never_a_direct_user_act`
  and `the_origin_check_agrees_with_core` in `crates/turnframe-test/tests/proptests.rs`;
  `origin_satisfies_rules`, `model_interpreted_answers_never_authorize_above_low_risk` and
  `each_confirmation_policy_accepts_only_its_own_authority` in
  `crates/turnframe-core/src/command.rs`;
  `a_consequential_act_waits_for_the_card_its_policy_names` in
  `crates/turnframe-runtime/tests/runtime_scenarios.rs`. Doc: ADR-001, ADR-014.
- [x] **No ambiguous target can execute a mutation.**
  Test: `two_open_cases_of_the_same_kind_produce_a_selection_card` in
  `crates/turnframe-runtime/tests/runtime_scenarios.rs`;
  `several_records_that_fit_leave_the_act_ambiguous` in
  `crates/turnframe-understand/tests/several_records_that_fit_leave_the_act_ambiguous.rs`;
  `several_records_found_are_offered_as_a_choice` in
  `crates/turnframe-runtime/tests/a_record_named_but_not_listed_is_looked_up.rs`;
  `ambiguity_raises_a_selection_card_and_leaves_the_other_act_alone` in
  `crates/turnframe-runtime/tests/reduce_scenarios.rs`. Doc: ADR-013. Metric:
  `turnframe.target.ambiguous`, emitted by the orchestrator.
- [x] **No stale interaction can execute.**
  Test: `a_stale_confirmation_after_an_edit_is_rejected` in
  `crates/turnframe-runtime/tests/runtime_scenarios.rs`; `stale_revision_rejected_both_ways` in
  `crates/turnframe-core/src/interaction.rs`;
  `a_second_answer_to_the_same_card_loses_the_compare_and_swap` in
  `crates/turnframe-store-postgres/tests/concurrency.rs`. Check:
  `check_begin_resolution_cas` and `check_revision_invalidation_respects_independence` in
  `crates/turnframe-store/src/conformance/interactions.rs`, run against every store. Metric:
  `turnframe.interaction.stale`, emitted by the orchestrator.
  The evidence is thinner than the gate deserves in one respect: the state explorer has no notion of
  a stale interaction, so staleness is proven by the runtime scenario and the store contract rather
  than across every reachable state of every workflow.
- [x] **No CTA meaning comes from a client-supplied value.**
  Test: `a_client_cannot_change_a_call_to_action_meaning_by_changing_a_value` in
  `crates/turnframe-runtime/tests/runtime_scenarios.rs`; `already_resolved_returns_original_option`,
  `an_option_that_authorizes_nothing_mints_no_origin` and
  `a_confirming_card_may_never_be_resolved_from_text` in `crates/turnframe-core/src/interaction.rs`;
  `a_selection_click_does_not_authorize_the_command_it_disambiguates` in
  `crates/turnframe-core/src/policy.rs`. Doc: ADR-004.
- [x] **No critical command lacks idempotency.**
  Test: `a_double_click_executes_at_most_once` in
  `crates/turnframe-runtime/tests/runtime_scenarios.rs`;
  `the_executor_replays_a_key_and_conflicts_on_a_stale_revision` in
  `crates/turnframe-test/tests/proptests.rs`; `a_key_is_admitted_once_under_any_interleaving` in
  `crates/turnframe-store/tests/journal_idempotency.rs`; `idempotency_key_is_stable_and_sensitive`
  in `crates/turnframe-core/tests/proptest_roundtrips.rs`. Check:
  `check_repeated_key_replays_the_outcome` and `check_repeated_key_with_another_command_is_refused`
  in `crates/turnframe-test/src/executors/idempotency.rs`, plus
  `check_journal_idempotency_replay` in `crates/turnframe-store/src/conformance/journal.rs`.
  Doc: ADR-006. Metric: `turnframe.command.idempotency_replay`, emitted by the orchestrator.
- [x] **No mutable case lacks revision checking.**
  Check: `check_stale_expected_revision_is_a_conflict` and
  `check_commit_reports_the_revision_it_reached` in
  `crates/turnframe-test/src/executors/revision.rs`,
  which every executor is run against. Test:
  `an_executor_that_misreports_its_revision_fails_exactly_one_check` in
  `crates/turnframe-test/tests/executor_conformance.rs` (the control that stops those checks passing
  vacuously); `an_untouched_batch_still_checks_the_revision` and
  `resuming_after_somebody_else_wrote_is_a_revision_conflict` in
  `crates/turnframe-test/tests/executor.rs`;
  `two_transactions_racing_the_same_expected_revision_only_one_wins` in
  `crates/turnframe-store-postgres/tests/concurrency.rs`. Doc: ADR-006. Metric:
  `turnframe.command.revision_conflict`, emitted by the orchestrator.
- [x] **No critical success receipt lacks committed event IDs.**
  Test: `a_failed_command_cannot_produce_a_resolved_looking_receipt` in
  `crates/turnframe-runtime/tests/runtime_scenarios.rs`;
  `nothing_that_states_an_outcome_is_streamed_before_the_commit` and
  `the_acknowledgement_arrives_whole_after_the_commit` in
  `crates/turnframe-runtime/tests/streaming_and_recovery.rs`; `outcome_claims_need_receipt_blocks`
  in `crates/turnframe-core/src/response.rs`; `a_receipt_with_no_events_is_refused` and
  `a_receipt_citing_an_uncommitted_event_is_refused` in `crates/turnframe-test/src/assertions.rs`;
  `a_trip_turn_passes_the_claim_guard`, `a_traveler_turn_passes_the_claim_guard` and
  `a_receipt_turn_passes_the_claim_guard` in
  `crates/turnframe-test/tests/claim_guard.rs`. Doc: ADR-005. Metric:
  `turnframe.claim.receipt_emitted`, emitted by the orchestrator.
- [x] **No external timeout is represented as a definite failure when the outcome may be unknown.**
  Test: `an_external_timeout_becomes_an_unknown_outcome_rather_than_a_blind_retry` in
  `crates/turnframe-runtime/tests/runtime_scenarios.rs`;
  `a_send_that_never_answers_becomes_an_unknown_outcome_and_not_a_retry`,
  `an_unknown_outcome_is_settled_by_the_reconciliation_hook` and `a_settled_row_is_never_sent_again`
  in `crates/turnframe-runtime/tests/outbox_dispatch.rs`;
  `an_unknown_external_outcome_is_reconciled_and_never_retried` in
  `crates/turnframe-runtime/tests/streaming_and_recovery.rs`;
  `a_crash_after_outbox_dispatch_leaves_a_row_a_reaper_can_release` in
  `crates/turnframe-runtime/tests/chaos.rs`. Doc: ADR-007. Metric:
  `turnframe.external.outcome_unknown` is emitted by the orchestrator;
  `turnframe.external.reconciled` is emitted by `dispatch` once per unknown outcome settled.
- [x] **No required interaction can be referenced before persistence.**
  Test: `failed_interaction_persistence_cannot_produce_text_referring_to_a_visible_card` in
  `crates/turnframe-runtime/tests/runtime_scenarios.rs`;
  `a_crash_after_interaction_persistence_never_doubles_the_card` in
  `crates/turnframe-runtime/tests/chaos.rs`. Check: `check_blocking_interaction_conflict` and
  `check_blocking_interaction_replace` in
  `crates/turnframe-store/src/conformance/interactions.rs`. Metric:
  `turnframe.interaction.failed`, emitted by `interactions` with the failure's code.
- [x] **No malformed multi-act response executes a subset.**
  Test: `a_malformed_second_act_causes_zero_acts_to_execute` in
  `crates/turnframe-runtime/tests/runtime_scenarios.rs`;
  `a_pointer_outside_the_message_is_repaired_with_the_exact_error` in
  `crates/turnframe-understand/tests/`, where a task's answer is taken whole or sent back whole.
  Check: the `Check::MalformedJson`,
  `Check::UnknownFields` and `Check::MissingRequiredFields` rows of
  `crates/turnframe-provider/src/conformance/report.rs`, run for every adapter (see the adapter
  conformance gate below for the entry test of each crate). Metric:
  `turnframe.task.repaired`, emitted by `turnframe-tasks` for every answer sent back.

## Workflow gates

- [x] **Exactly one phase for every generated reachable state.**
  Test: `trip_reachable_states_satisfy_every_invariant`,
  `traveler_reachable_states_satisfy_every_invariant`,
  `claim_reachable_states_satisfy_every_invariant`, `every_sample_projection_carries_one_phase` and
  `one_case_resolves_to_the_same_phase_in_two_projections` in
  `crates/turnframe-test/tests/exploration.rs`, with
  `two_projections_that_disagree_about_the_phase_are_reported` as the control that proves the check
  can fail. Invariant: `check_view` in `crates/turnframe-core/src/flow/invariants.rs`, plus
  `single_phase` and `same_phase_in` in `crates/turnframe-test/src/assertions.rs`. Doc: ADR-003.
  One phase per view is structural (`WorkflowView::phase` is a single value by type), so what these
  checks add is that the erased form never smuggles a null or a list through the serializer, and
  that two projectors agree about the same case.
- [x] **Parameterized obligations cover repeated entities.**
  Test: `obligations_are_parameterized_per_extra` in
  `crates/turnframe-test/src/workflows/trip/apply.rs`; `erase_produces_stable_obligation_ids` in
  `crates/turnframe-core/src/flow/mod.rs`;
  `a_field_the_document_did_not_yield_is_a_different_obligation` in
  `crates/turnframe-test/tests/receipt_claim.rs`;
  `the_trip_model_fills_the_extra_budget_and_revisits_the_second_traveler` in
  `crates/turnframe-test/tests/exploration.rs`, which drives the extra budget to its limit.
  Invariant: `InvariantViolationKind::DuplicateObligation`, proven reportable by
  `duplicate_obligations_and_non_blocking_slot` in `crates/turnframe-core/src/flow/invariants.rs`.
- [x] **User phases derive blocking interactions.**
  Invariant: `InvariantViolationKind::MissingBlockingInteraction`,
  `BlockingInteractionOnTerminalPhase`, `BlockingInteractionOnNonUserPhase` and
  `NonBlockingRequirementInBlockingSlot`, proven reportable by `user_phase_without_interaction` and
  `outcome_on_non_terminal_and_non_user_blocking` in
  `crates/turnframe-core/src/flow/invariants.rs`, and enforced over every reachable state by the
  three `*_reachable_states_satisfy_every_invariant` tests in
  `crates/turnframe-test/tests/exploration.rs` (which also report
  `ExplorationViolationKind::BlockingInteractionNotBuildable` and `…NotAnswerable`). Test:
  `a_second_blocking_card_replaces_and_invalidates_the_first` in
  `crates/turnframe-runtime/src/interactions.rs`;
  `non_blocking_cards_never_take_the_blocking_slot` in
  `crates/turnframe-store/src/memory/impls.rs`.
  "At most one blocking interaction" is structural rather than a property test: the view holds an
  `Option`, so a second one cannot be represented.
- [x] **Terminal outcomes are explicit and domain-correct.**
  Invariant: `InvariantViolationKind::TerminalPhaseWithoutOutcome`, `OutcomeOnNonTerminalPhase` and
  `OutcomeWithObligations`, checked over every reachable state by the three
  `*_reachable_states_satisfy_every_invariant` tests, and proven reportable by
  `outcome_on_non_terminal_and_non_user_blocking` in
  `crates/turnframe-core/src/flow/invariants.rs`. Test: `an_outcome_no_path_reaches_is_reported`,
  `a_projector_that_ends_a_case_by_disappearing_is_reported`,
  `a_transition_that_drops_the_case_is_reported` and
  `no_sample_workflow_ends_a_case_by_disappearing` in
  `crates/turnframe-test/tests/exploration.rs`.
- [x] **Projection behavior is versioned.**
  What exists: the workflow version is recorded per turn as `ReplayRecord::workflow_versions`,
  written by `crates/turnframe-runtime/src/orchestrator/session/load.rs` and read into the trace by
  `crates/turnframe-telemetry/src/tracing.rs`, and the rule is stated in ADR-002 and in
  `docs/architecture.md` §4 ("Projection behaviour is versioned").
  Test: `crates/turnframe-test/tests/projection_versioning.rs` snapshots each sample projector under
  a name carrying its version, so a change without a bump fails the recorded file and a change with
  a bump writes a new one beside it. `a_behaviour_change_under_a_fixed_version_moves_the_pin` and
  `a_rewording_does_not_move_the_pin` hold the mechanism itself, since a pin that never moves is
  indistinguishable from a pin that is not attached. Adopters reach it through
  `turnframe_test::projection::ProjectionFingerprint`.
- [x] **State exploration finds no dead end lacking an explicit user, system, or external trigger.**
  Invariant: `ExplorationViolationKind::DeadEnd`, enforced by the three
  `*_reachable_states_satisfy_every_invariant` tests in
  `crates/turnframe-test/tests/exploration.rs` and proven reportable by
  `a_state_with_nowhere_to_go_is_reported_as_a_dead_end`. Metric:
  `turnframe.workflow.invariant_violation`, emitted by the orchestrator (must be zero in every
  pre-release run).

## Conversation gates

- [x] **Text and an interaction response can coexist.**
  Test: `a_card_response_and_text_coexist_in_one_turn` in
  `crates/turnframe-runtime/tests/runtime_scenarios.rs`. Doc: `docs/interactions.md`.
- [x] **Multi-action turns are supported.**
  Test: `one_message_sets_several_independent_fields_atomically` and
  `a_database_timeout_during_an_atomic_batch_leaves_no_hidden_partial_state` in
  `crates/turnframe-runtime/tests/runtime_scenarios.rs`;
  `independent_fields_on_one_case_commit_together` in
  `crates/turnframe-runtime/tests/reduce_scenarios.rs`. Check:
  `check_per_case_batch_is_all_or_nothing` in `crates/turnframe-test/src/executors/batch.rs` and
  `check_commit_bundle_atomic_on_invalid_item` in
  `crates/turnframe-store/src/conformance/commit_bundle.rs`.
- [x] **Action + question works in either order.**
  Test: `an_action_and_a_question_both_receive_results` in
  `crates/turnframe-runtime/tests/runtime_scenarios.rs` and
  `an_action_and_a_question_are_both_answered` in
  `crates/turnframe-runtime/tests/reduce_scenarios.rs`, plus
  `shuffling_independent_acts_keeps_the_command_set` in
  `crates/turnframe-runtime/tests/reduce_proptests.rs` for the ordering of acts among themselves.
  The evidence is weaker than the gate sounds: neither test runs both orderings of an act and a
  question in one message.
- [x] **Questions cannot disappear behind actions.**
  Test: `a_question_unsupported_by_current_sources_stays_explicit` in
  `crates/turnframe-runtime/tests/runtime_scenarios.rs`;
  `three_questions_get_three_answers_in_the_order_asked` and
  `a_question_left_unanswered_says_so` in
  `crates/turnframe-runtime/tests/answers.rs`. Metric: `turnframe.question.answered` and
  `turnframe.question.unanswered`, both emitted by the orchestrator.
- [x] **Out-of-order data is accepted when domain-valid.**
  Test: `shuffling_independent_acts_keeps_the_command_set`,
  `reducing_the_same_turn_twice_gives_the_same_plan_hash` and
  `every_act_keeps_a_slot_and_every_command_keeps_a_decision` in
  `crates/turnframe-runtime/tests/reduce_proptests.rs`; `trip_projection_is_deterministic` in
  `crates/turnframe-test/tests/proptests.rs`.
- [x] **Corrections and negations are resolved before effects.**
  Test: `a_correction_that_leaves_the_value_unchanged_causes_no_mutation` in
  `crates/turnframe-runtime/tests/runtime_scenarios.rs`;
  `a_correction_replaces_the_act_it_corrects` and `a_cancel_withdraws_the_act_it_names` in
  `crates/turnframe-understand/tests/`; `a_second_value_for_one_subject_is_still_a_correction` and
  `one_operation_used_twice_about_two_subjects_runs_twice` in
  `crates/turnframe-runtime/tests/supersession.rs`;
  `do_not_submit_blocks_the_rebooking_and_nothing_else` in
  `crates/turnframe-runtime/tests/reduce_scenarios.rs`; the standing property is
  `every_reduction_passes_the_plans_own_consistency_rules` in the same file. Doc: ADR-014.
- [x] **The response remains natural and localized.**
  Test: the corpus runs against a real endpoint in
  `the_corpus_runs_against_a_real_model` in `crates/turnframe-eval/tests/live_corpus.rs`, against
  any of the three shipped vendors, and it skips with a printed note rather than passing quietly
  when no credential is set. Its 76 items are in English and Italian (30 declare `it-IT`). In
  the scripted corpus, `set_name_italian.toml` asserts the receipt comes back in the language
  the turn declared, because a turn answered correctly in the wrong language is still wrong. The harness's type system keeps a judge
  away from effects (a `JudgeInput` is two strings), proven by
  `a_side_effect_failure_is_not_averaged_into_a_language_score` in
  `crates/turnframe-eval/src/report.rs`; localized composition is covered by
  `receipts_are_localized_in_italian_and_english` in `crates/turnframe-test/tests/claim_guard.rs`
  and `an_untranslated_locale_keeps_the_shipped_sentence` in
  `crates/turnframe-runtime/tests/runtime_refusals.rs`. Doc: ADR-010 and `docs/composition.md`.
  The measured rates are in [benchmarks](benchmarks.md), with the caveats that belong to one
  sample per item.

## Provider gates

- [x] **Capability routing is explicit.**
  Test: `capability_fit_runs_before_policy_and_is_never_relaxed` and
  `no_silent_downgrade_names_what_was_missing` in `crates/turnframe-provider/src/router.rs`;
  `satisfied_by_reports_every_gap_in_order` and
  `context_window_is_fail_closed_and_compared_numerically` in
  `crates/turnframe-provider/src/capabilities.rs`;
  `capability_mismatch_names_the_missing_transport` in `crates/turnframe-provider/src/error.rs`.
  Doc: ADR-008, `docs/provider-adapters.md`. Metric: `turnframe.provider.capability_mismatch`,
  emitted by `turnframe-tasks` once per candidate routing refused.
- [x] **Critical stages reject unsupported structured-output modes.**
  Check: the `Check::NoSilentCapabilityDowngrade` row of
  `crates/turnframe-provider/src/conformance/report.rs`, run for every adapter. Test:
  `a_json_object_profile_does_not_quietly_upgrade_itself` in
  `crates/turnframe-provider-openai/tests/conformance.rs` and
  `crates/turnframe-provider-gemini/tests/conformance.rs`;
  `a_prompt_only_profile_does_not_quietly_upgrade_itself` in
  `crates/turnframe-provider-anthropic/tests/conformance.rs`;
  `a_transport_converse_does_not_have_is_refused_rather_than_downgraded` in
  `crates/turnframe-provider-bedrock/src/wire/request.rs`;
  `an_undeclared_capability_is_a_mismatch_before_a_byte_leaves` in
  `crates/turnframe-provider-gemini/src/wire/request.rs`;
  `a_profile_without_vision_refuses_an_image_as_a_capability_mismatch` in
  `crates/turnframe-provider-ollama/src/wire/request.rs`;
  `a_schema_the_grammar_cannot_express_is_refused_before_the_wire` in
  `crates/turnframe-provider-openai/tests/conformance.rs`.
- [x] **Fallback never repeats a possibly committed command.**
  Test: `moving_on_to_another_candidate_is_reported_as_a_fallback` in
  `crates/turnframe-runtime/tests/signals.rs`;
  `a_provider_failure_after_commit_regenerates_narration_without_repeating_commands` in
  `crates/turnframe-runtime/tests/runtime_scenarios.rs`;
  `a_critical_purpose_is_refused_after_commit`,
  `partial_outputs_from_different_providers_are_never_merged` and
  `the_stage_decides_which_purposes_may_run` in `crates/turnframe-provider/src/fallback.rs`.
  Metric: `turnframe.provider.fallback`, emitted by `turnframe-tasks` once per attempt that moved on.
- [x] **Adapter conformance suites pass.**
  Check: `Check::run_order` in `crates/turnframe-provider/src/conformance/report.rs`, with twenty
  feature rows (valid response, malformed JSON, unknown fields, missing fields, multiple acts,
  read request ids, streaming reconstruction, streaming increments, streaming usage agreement, token
  usage contract, empty output, refusal, timeout, rate limit, authentication failure, context
  overflow, cancellation, retry classification, redaction, no silent downgrade) plus thirteen
  per-status rows, run against `wiremock` fixtures. Test: the entry test of each adapter crate:
  `the_openai_profile_passes_every_row_without_skipping_one`,
  `the_anthropic_profile_passes_every_row_without_skipping_one`,
  `the_developer_api_profile_passes_every_row_without_skipping_one` (gemini),
  `a_measured_profile_passes_every_row_without_skipping_one` (bedrock) and
  `a_bare_local_daemon_passes_every_row_it_can_produce` (ollama), each living in the
  `crates/turnframe-provider-<vendor>/tests/conformance.rs` of its own crate; the kit itself is
  checked by `an_adapter_that_flattens_its_errors_is_caught` and
  `an_undeclared_skip_is_reported_as_unproven` in
  `crates/turnframe-test/tests/provider_conformance.rs`. Live smoke tests exist as the
  `crates/turnframe-provider-<vendor>/tests/live_smoke.rs` of each adapter crate and skip when `CI`
  is set or no credential is present.
  The half of this gate that says they are "recorded in the release notes" cannot be closed until
  there are release notes to record them in.
- [x] **Provider raw data and secrets are redacted.**
  Check: the `Check::SecretRedaction` row of `crates/turnframe-provider/src/conformance/report.rs`.
  Test: `api_key_never_renders_its_value`, `fingerprint_identifies_without_revealing`,
  `registered_literals_are_masked_anywhere` and
  `redaction_preserves_everything_that_is_not_a_secret` in
  `crates/turnframe-provider/src/secret.rs`; `the_key_appears_in_no_rendering_of_the_adapter` in
  `crates/turnframe-provider-openai/src/provider.rs` and
  `crates/turnframe-provider-anthropic/src/provider.rs`;
  `a_failing_call_never_renders_the_configured_key` in
  `crates/turnframe-provider-openai/tests/conformance.rs`;
  `debug_output_carries_a_fingerprint_and_never_the_secret` in
  `crates/turnframe-prompt/src/langfuse.rs`. Doc: `docs/threat-model.md`, `SECURITY.md`.

## Operational gates

- [x] **Crash recovery is tested at every commit boundary.**
  Test: the seven boundaries of `CRASH_BOUNDARIES` in `crates/turnframe-test/src/stores/mod.rs`, one
  chaos test each in `crates/turnframe-runtime/tests/chaos.rs`:
  `a_crash_after_interaction_persistence_never_doubles_the_card`,
  `a_crash_before_the_journal_insert_recovers_by_simply_running_again`,
  `a_crash_after_the_journal_insert_resumes_by_idempotency_key`,
  `a_crash_inside_the_commit_bundle_leaves_the_ledger_empty_and_recovers`,
  `a_crash_before_outbox_dispatch_leaves_the_row_claimable`,
  `a_crash_after_outbox_dispatch_leaves_a_row_a_reaper_can_release` and
  `a_crash_before_response_persistence_keeps_the_effects_and_regenerates`, with
  `every_boundary_of_the_specification_has_a_name` asserting the table is complete, and
  `a_before_boundary_leaves_the_key_free` and
  `an_after_boundary_leaves_the_entry_for_recovery_to_find` in
  `crates/turnframe-test/tests/fake_stores.rs` proving the injection itself works.
  Two of the boundaries the specification lists are not injectable store failures and are covered
  separately: after the remote request by
  `a_send_that_never_answers_becomes_an_unknown_outcome_and_not_a_retry` in
  `crates/turnframe-runtime/tests/outbox_dispatch.rs`, and during streaming by
  `a_committed_turn_regenerates_its_answer_without_executing_anything`,
  `a_turn_that_never_admitted_a_command_starts_over` and
  `a_finished_turn_needs_no_recovery` in
  `crates/turnframe-runtime/tests/streaming_and_recovery.rs`, all of which resume from the persisted
  `TurnPhase`.
- [x] **Live response and reload use the same persisted blocks.**
  Test: `a_reload_returns_the_same_ordered_blocks_and_interaction_state` in
  `crates/turnframe-runtime/tests/runtime_scenarios.rs`. Check:
  `check_conversation_turn_persistence` in
  `crates/turnframe-store/src/conformance/conversations.rs`. Doc: ADR-010.
- [x] **Audit records reconstruct command authorization and claims.**
  What exists: `ReplayEvidence::explains_its_turn` in `crates/turnframe-test/src/replay.rs` checks
  that every command has a policy decision, a recorded origin that satisfies that decision, that no
  refused command was committed, and that every receipt cites an event the record lists, exercised
  by `a_record_built_from_a_real_commit_explains_its_turn`,
  `a_receipt_that_outruns_the_ledger_is_reported` and
  `a_low_risk_command_needs_no_confirmation_for_the_record_to_hold` in
  `crates/turnframe-test/tests/scripted_turn.rs`. The record round-trips through storage via
  `check_replay_put_get` in `crates/turnframe-store/src/conformance/replay.rs`.
  Test: `crates/turnframe-runtime/tests/audit_record.rs` runs an ordinary turn, discards everything
  it returned, reads the record back from the store by turn identifier alone, and answers the
  specification's ten questions from that: `the_record_answers_what_the_user_sent`,
  `the_record_answers_which_state_and_revision_were_loaded`,
  `the_record_answers_what_the_model_understood_and_what_grounded_it`,
  `the_record_answers_which_model_calls_produced_the_understanding`,
  `the_record_answers_how_the_target_was_resolved`,
  `the_record_answers_which_policy_applied_and_what_authorized_the_command`,
  `the_record_answers_what_committed_and_which_event_backs_each_receipt`,
  `the_record_answers_what_was_returned_to_the_user`, `the_record_says_how_far_the_turn_got`, and
  `one_stored_record_answers_the_whole_audit_list`, which asks all ten at once so a field that stops
  being populated fails there rather than in whichever question happened to cover it.
- [x] **Metrics separate safety failures from language-quality failures.**
  Test: `the_dashboard_has_every_panel_of_26_3_once`, `integrity_and_semantics_are_never_merged`,
  `every_safety_signal_appears_on_an_alerting_panel`, `every_series_is_a_metric_this_crate_emits`
  and `the_default_dashboard_is_the_reliability_one` in
  `crates/turnframe-telemetry/src/dashboard.rs`, which define the seven panels (side-effect
  integrity, claim integrity, semantic interpretation, clarification rate, abandonment rate,
  provider failures, user-experience scores) as data with no single "accuracy" series;
  `a_side_effect_failure_is_not_averaged_into_a_language_score` in
  `crates/turnframe-eval/src/report.rs` keeps the same separation in the evaluation report.
  Every panel is now fed: the sixteen signals that were declared and emitted by nothing, including
  all seven latency series, fire from the pipeline as of `tests/signals.rs` in
  `crates/turnframe-runtime`, where each is driven through the real path and asserted with its whole
  label set. `every_declared_signal_is_driven_by_this_suite` in that file unions what the observer
  actually saw and requires it to cover the vocabulary, so a signal cannot be declared and left
  unemitted again. One exemption stands and is a finding rather than a convenience:
  `turnframe.command.idempotency_replay` cannot fire, because the executor replays the original
  outcome rather than producing the replay variant the signal maps from, which makes that arm
  unreachable. One metric is partly blind: the router surfaces its rejection list only when it
  admits no candidate at all, so `turnframe.provider.capability_mismatch` cannot fire on a run where
  a weaker profile was refused and a stronger one answered.
- [x] **Canary and rollback exist per workflow.**
  Doc: `docs/canary-and-rollback.md`, which states the three-stage canary and the three rules that
  make a rollback safe, and says plainly which half is procedure rather than something a test can
  hold anyone to. Test: `crates/turnframe-store-postgres/tests/rollback.rs` exercises the stored
  half against a live database:
  `the_previous_version_reads_a_case_the_newer_one_wrote`,
  `a_rollback_neither_rewrites_nor_removes_an_event`,
  `a_card_the_newer_version_left_open_is_still_answerable_afterwards`,
  `the_other_half_of_the_rule_is_invalidating_the_cards_instead` and
  `rolling_one_workflow_back_leaves_another_alone`. Check:
  `check_administrative_invalidation_ignores_the_revision` in
  `crates/turnframe-store/src/conformance/interactions.rs`, which holds every store to the
  withdrawal path a rollback needs. The shadow stage the canary runs on is
  `crates/turnframe-runtime/src/divergence.rs`, tested by
  `crates/turnframe-runtime/tests/divergence_vocabulary.rs`.
  Writing the document surfaced a gap in the contract: revision-driven invalidation fires only for a
  card bound to a revision the case has left, so the withdrawal step the rollback prescribes was not
  expressible until `InteractionWriter::invalidate_case_cards` was added.

## Mechanical release steps

Perform these in order once every gate above is checked.

1. **Quality bar.** `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`,
   `cargo test --workspace`, `cargo deny check`, `cargo audit`. Doc examples compile as tests. All
   five run today in `.github/workflows/ci.yml`, with a PostgreSQL 16 service so the
   `turnframe-store-postgres` tests execute rather than skip.
2. **Semver review.** Run `cargo-semver-checks` on every publishable crate against the previous
   release. Any breaking change either bumps the major/minor component according to semver
   for pre-1.0 crates or is reverted. Confirm unstable modules are still labelled as such.
   The CI job exists but is `continue-on-error` and has no baseline: until a version is on
   crates.io it validates manifests only, so this step is genuinely first meaningful at the second
   release.
3. **CHANGELOG.** Update `CHANGELOG.md` with a section for the new version: added, changed,
   deprecated, removed, fixed, security. Mention migration steps for any breaking change. Changes
   made since the last release gather under `[Unreleased]` until then.
4. **Version bump.** Set `workspace.package.version` and every intra-workspace dependency
   version in the root `Cargo.toml`; refresh `Cargo.lock`.
5. **Tag.** Create an annotated tag `vX.Y.Z` on the reviewed commit.
6. **Publish in dependency order.** `cargo publish --workspace --dry-run` first, then
   `cargo publish --workspace`: cargo orders the eighteen library crates by their dependencies and
   waits for each to be indexed before the next, and the four examples are `publish = false`. The
   order it takes is `turnframe-core` and `turnframe-macros`; `turnframe-provider`,
   `turnframe-store` and `turnframe-telemetry`; `turnframe-prompt`, `turnframe-tasks`,
   `turnframe-store-postgres` and the Bedrock and Ollama adapters; `turnframe-understand`; the
   OpenAI, Anthropic and Gemini adapters, whose live tests use it; `turnframe-test` and
   `turnframe-runtime`; `turnframe-eval`; and last the `turnframe` facade.
7. **Release notes.** Attach the CHANGELOG section, the conformance compatibility table produced by
   `ConformanceReport::compatibility_table` in `crates/turnframe-provider/src/conformance/report.rs`
   from an actual run, and the live smoke-test summary.

## Performance claims

No public statement about latency, throughput or overhead is made in the README, docs, release
notes or crate metadata until a benchmark report exists. The report must come from the
`criterion` benchmarks for workflow projection, reduction and persistence overhead, and must
report each of these separately from provider latency, external command latency and narration
latency. Until then the only permitted wording is the qualitative one from the specification:
projection and reduction are designed to be negligible relative to network I/O. Reliability
figures follow the same rule: they come from observed production metrics, never from
expectations.

Two facts about the current state, so that nobody reads a report into this section that does not
exist. The benchmarks that do exist are `projection` and `hashing` in
`crates/turnframe-core/benches`,
`persistence` in `crates/turnframe-store/benches`, and `structured_parsing` and
`stream_reconstruction` in `crates/turnframe-provider/benches`; there is no reduction benchmark yet,
as `docs/benchmarks.md` says. And continuous integration only compiles them
(`cargo bench --workspace --no-run`), so no run has produced a number anybody has kept.
