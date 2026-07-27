export function completionWarnings({
  jobWarnings = [],
  extraction,
  emptyResponseWarning,
  extractionWarnings = [],
  finalityAnchor,
  domOnlyFinalityWarning
}) {
  return [
    ...(jobWarnings ?? []),
    ...(extraction?.text ? [] : [emptyResponseWarning]),
    ...(extraction?.warning ? [extraction.warning] : []),
    ...(extractionWarnings ?? []),
    ...(finalityAnchor === "dom_only" ? [domOnlyFinalityWarning] : [])
  ];
}
