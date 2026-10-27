//! What validate says about a project's SVG pictures (vector resources):
//! refused hrefs, a file over the caps, text that will not draw, fonts the
//! import could not find — read from the files by promo-svg. The one call
//! `promo validate` and the app's `promo_validate` (through the FFI's
//! report) both make, so the two doors cannot disagree.

use crate::project::Project;

/// The vector findings, added to a report as warnings.
pub fn report_into(report: &mut promo_timeline::report::Report, project: &Project) {
    for finding in promo_svg::findings(&project.meta, &project.dir.join("Resources")) {
        report.warn(finding);
    }
}
