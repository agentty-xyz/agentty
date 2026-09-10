use crate::assertion::AssertionFailure;
use crate::frame::TerminalFrame;
use crate::proof::report::ProofReport;

/// Build a report with one capture and one structured failure routed
/// through `record_soft_failure`, mirroring the
/// `SoftAssertions::with_report` flow.
pub(crate) fn report_with_structured_failure(failure: &AssertionFailure) -> ProofReport {
    let frame = TerminalFrame::new(20, 3, b"Hello World");
    let mut report = ProofReport::new("structured_failure");
    report.add_capture("only", "Only capture", &frame);
    report.record_soft_failure(failure);

    report
}
