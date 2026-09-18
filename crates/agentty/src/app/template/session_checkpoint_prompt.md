Produce a compact checkpoint with these labels, omitting empty sections: Objective;
Constraints and accepted decisions; Completed; Remaining; Checks; Uncertainty; Sources.
Preserve explicit cancellations and objective replacements, pending questions, and next
actions. Keep plans separate from completed work. For each check retain its exact
command, observed result, source state or subsequent invalidating changes, and evidence
reference when supplied. Unknown results stay unverified. Successful checks may be
reused only while relevant inputs remain unchanged. Preserve these distinctions when
reducing earlier summaries; never upgrade an assistant claim into tool evidence.

Example: "Assistant says tests passed" becomes "Checks: unverified claim". Example:
"cargo test exited 0; then parser changed" becomes "Checks: cargo test passed before
parser change; affected checks need rerun".
