use crate::backend::{capture_kind_arg, resolve_target};
use control::{AgentError, TargetSelector};
use domain::{CaptureSourceKind, CaptureTarget, RecorderSettings};

pub(crate) fn resolve_record_target(
    targets: &[CaptureTarget],
    kind: CaptureSourceKind,
    selector: Option<&TargetSelector>,
    saved_id: Option<u64>,
) -> Result<CaptureTarget, AgentError> {
    match selector {
        Some(TargetSelector::Id { kind, id }) => {
            resolve_target(targets, *kind, Some(*id), None).map_err(target_error)
        }
        Some(TargetSelector::Name { kind, query }) => {
            let candidates = targets
                .iter()
                .filter(|target| kind.map_or(true, |kind| target.kind == kind))
                .collect::<Vec<_>>();
            resolve_by_name(candidates, query, "target")
        }
        Some(TargetSelector::App { query }) => {
            let candidates = targets
                .iter()
                .filter(|target| target.kind == CaptureSourceKind::Window)
                .collect::<Vec<_>>();
            resolve_by_app(candidates, query)
        }
        None => resolve_target(targets, kind, None, saved_id).map_err(target_error),
    }
}

pub(crate) fn settings_for_target(
    mut settings: RecorderSettings,
    target: &CaptureTarget,
) -> RecorderSettings {
    settings.source = target.kind;
    settings
}

fn resolve_by_name(
    candidates: Vec<&CaptureTarget>,
    query: &str,
    label: &str,
) -> Result<CaptureTarget, AgentError> {
    let query = normalized(query);
    if query.is_empty() {
        return Err(AgentError {
            code: "empty_target_query".into(),
            message: format!("{label} query cannot be empty"),
            recoverable: true,
            next: "Pass a non-empty target name or use `wrec targets --json` to choose an id."
                .into(),
        });
    }

    for predicate in [MatchKind::Exact, MatchKind::Prefix, MatchKind::Contains] {
        let matches = candidates
            .iter()
            .copied()
            .filter(|target| match predicate {
                MatchKind::Exact => normalized(&target.name) == query,
                MatchKind::Prefix => normalized(&target.name).starts_with(&query),
                MatchKind::Contains => normalized(&target.name).contains(&query),
            })
            .collect::<Vec<_>>();
        if !matches.is_empty() {
            return unique_match(matches, label, &query);
        }
    }

    Err(AgentError {
        code: "target_not_found".into(),
        message: format!("no {label} matches `{query}`"),
        recoverable: true,
        next: "Run `wrec targets --json` and pass `--target kind:id` for an exact target.".into(),
    })
}

fn resolve_by_app(
    candidates: Vec<&CaptureTarget>,
    query: &str,
) -> Result<CaptureTarget, AgentError> {
    let query = normalized(query);
    if query.is_empty() {
        return Err(AgentError {
            code: "empty_app_query".into(),
            message: "app query cannot be empty".into(),
            recoverable: true,
            next: "Pass an app name or use `wrec targets --json` to choose a window id.".into(),
        });
    }

    for predicate in [MatchKind::Exact, MatchKind::Prefix, MatchKind::Contains] {
        let matches = candidates
            .iter()
            .copied()
            .filter(|target| match predicate {
                MatchKind::Exact => normalized(app_name(target)) == query,
                MatchKind::Prefix => normalized(app_name(target)).starts_with(&query),
                MatchKind::Contains => normalized(app_name(target)).contains(&query),
            })
            .collect::<Vec<_>>();
        if !matches.is_empty() {
            return unique_match(matches, "app", &query);
        }
    }

    Err(AgentError {
        code: "app_not_found".into(),
        message: format!("no app matches `{query}`"),
        recoverable: true,
        next: "Run `wrec targets --json` and pass `--target window:id` for an exact window.".into(),
    })
}

enum MatchKind {
    Exact,
    Prefix,
    Contains,
}

fn unique_match(
    matches: Vec<&CaptureTarget>,
    label: &str,
    query: &str,
) -> Result<CaptureTarget, AgentError> {
    match matches.as_slice() {
        [target] => Ok((*target).clone()),
        _ => Err(AgentError {
            code: "ambiguous_target".into(),
            message: format!(
                "multiple {label}s match `{query}`: {}",
                matches
                    .iter()
                    .map(|target| {
                        format!(
                            "{}:{} {}",
                            capture_kind_arg(target.kind),
                            target.id,
                            target.name
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            recoverable: true,
            next: "Pass `--target kind:id` to choose one exact target.".into(),
        }),
    }
}

fn normalized(value: &str) -> String {
    value.trim().to_lowercase()
}

fn app_name(target: &CaptureTarget) -> &str {
    target
        .name
        .split_once(" \u{2014} ")
        .map(|(app, _)| app)
        .unwrap_or(&target.name)
}

fn target_error(message: String) -> AgentError {
    AgentError {
        code: "target_not_found".into(),
        message,
        recoverable: true,
        next: "Run `wrec targets --json` and pass one of the listed target ids.".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::Resolution;

    fn display(id: u64, name: &str) -> CaptureTarget {
        CaptureTarget {
            id,
            name: name.into(),
            kind: CaptureSourceKind::Display,
        }
    }

    fn window(id: u64, name: &str) -> CaptureTarget {
        CaptureTarget {
            id,
            name: name.into(),
            kind: CaptureSourceKind::Window,
        }
    }

    fn resolve(
        targets: &[CaptureTarget],
        selector: Option<TargetSelector>,
    ) -> Result<CaptureTarget, AgentError> {
        resolve_record_target(targets, CaptureSourceKind::Display, selector.as_ref(), None)
    }

    #[test]
    fn id_selector_resolves_matching_target() {
        let targets = [display(1, "Built-in"), window(7, "Notes")];

        let resolved = resolve(
            &targets,
            Some(TargetSelector::Id {
                kind: CaptureSourceKind::Window,
                id: 7,
            }),
        )
        .unwrap();

        assert_eq!(resolved.id, 7);
        assert_eq!(resolved.kind, CaptureSourceKind::Window);
    }

    #[test]
    fn id_selector_with_unknown_id_fails_as_target_not_found() {
        let targets = [display(1, "Built-in")];

        let error = resolve(
            &targets,
            Some(TargetSelector::Id {
                kind: CaptureSourceKind::Display,
                id: 99,
            }),
        )
        .unwrap_err();

        assert_eq!(error.code, "target_not_found");
        assert!(error.recoverable);
    }

    #[test]
    fn name_selector_prefers_exact_match_over_prefix_match() {
        let targets = [window(1, "Notes"), window(2, "Notes Beta")];

        let resolved = resolve(
            &targets,
            Some(TargetSelector::Name {
                kind: None,
                query: "notes".into(),
            }),
        )
        .unwrap();

        assert_eq!(resolved.id, 1);
    }

    #[test]
    fn name_selector_prefers_prefix_match_over_contains_match() {
        let targets = [window(1, "My Notes"), window(2, "Notebook")];

        let resolved = resolve(
            &targets,
            Some(TargetSelector::Name {
                kind: None,
                query: "note".into(),
            }),
        )
        .unwrap();

        assert_eq!(resolved.id, 2);
    }

    #[test]
    fn name_selector_falls_back_to_contains_match() {
        let targets = [window(1, "My Notes"), window(2, "Terminal")];

        let resolved = resolve(
            &targets,
            Some(TargetSelector::Name {
                kind: None,
                query: "note".into(),
            }),
        )
        .unwrap();

        assert_eq!(resolved.id, 1);
    }

    #[test]
    fn name_selector_normalizes_case_and_whitespace() {
        let targets = [display(1, "Built-in Retina Display")];

        let resolved = resolve(
            &targets,
            Some(TargetSelector::Name {
                kind: None,
                query: "  BUILT-IN RETINA DISPLAY  ".into(),
            }),
        )
        .unwrap();

        assert_eq!(resolved.id, 1);
    }

    #[test]
    fn name_selector_filters_candidates_by_kind() {
        let targets = [display(1, "Main"), window(2, "Main")];

        let resolved = resolve(
            &targets,
            Some(TargetSelector::Name {
                kind: Some(CaptureSourceKind::Window),
                query: "main".into(),
            }),
        )
        .unwrap();

        assert_eq!(resolved.id, 2);
    }

    #[test]
    fn ambiguous_name_match_lists_candidate_ids() {
        let targets = [window(1, "Notes"), window(2, "notes")];

        let error = resolve(
            &targets,
            Some(TargetSelector::Name {
                kind: None,
                query: "notes".into(),
            }),
        )
        .unwrap_err();

        assert_eq!(error.code, "ambiguous_target");
        assert!(error.message.contains("window:1"));
        assert!(error.message.contains("window:2"));
    }

    #[test]
    fn blank_name_query_fails() {
        let targets = [display(1, "Built-in")];

        let error = resolve(
            &targets,
            Some(TargetSelector::Name {
                kind: None,
                query: "   ".into(),
            }),
        )
        .unwrap_err();

        assert_eq!(error.code, "empty_target_query");
    }

    #[test]
    fn unmatched_name_query_fails_as_target_not_found() {
        let targets = [display(1, "Built-in")];

        let error = resolve(
            &targets,
            Some(TargetSelector::Name {
                kind: None,
                query: "zelda".into(),
            }),
        )
        .unwrap_err();

        assert_eq!(error.code, "target_not_found");
    }

    #[test]
    fn app_selector_matches_app_name_before_separator() {
        let targets = [
            window(1, "Safari \u{2014} Apple"),
            window(2, "Notes \u{2014} Draft"),
        ];

        let resolved = resolve(
            &targets,
            Some(TargetSelector::App {
                query: "safari".into(),
            }),
        )
        .unwrap();

        assert_eq!(resolved.id, 1);
    }

    #[test]
    fn app_selector_uses_full_name_when_title_has_no_separator() {
        let targets = [window(1, "Terminal")];

        let resolved = resolve(
            &targets,
            Some(TargetSelector::App {
                query: "terminal".into(),
            }),
        )
        .unwrap();

        assert_eq!(resolved.id, 1);
    }

    #[test]
    fn app_selector_ignores_displays() {
        let targets = [display(1, "Safari"), window(2, "Safari \u{2014} Apple")];

        let resolved = resolve(
            &targets,
            Some(TargetSelector::App {
                query: "safari".into(),
            }),
        )
        .unwrap();

        assert_eq!(resolved.id, 2);
    }

    #[test]
    fn blank_app_query_fails() {
        let targets = [window(1, "Safari \u{2014} Apple")];

        let error = resolve(&targets, Some(TargetSelector::App { query: "".into() })).unwrap_err();

        assert_eq!(error.code, "empty_app_query");
    }

    #[test]
    fn unmatched_app_query_fails_as_app_not_found() {
        let targets = [window(1, "Safari \u{2014} Apple")];

        let error = resolve(
            &targets,
            Some(TargetSelector::App {
                query: "zelda".into(),
            }),
        )
        .unwrap_err();

        assert_eq!(error.code, "app_not_found");
    }

    #[test]
    fn no_selector_uses_saved_target_id() {
        let targets = [display(1, "Built-in"), display(2, "External")];

        let resolved =
            resolve_record_target(&targets, CaptureSourceKind::Display, None, Some(2)).unwrap();

        assert_eq!(resolved.id, 2);
    }

    #[test]
    fn no_selector_falls_back_to_first_target_when_saved_id_is_stale() {
        let targets = [display(1, "Built-in"), display(2, "External")];

        let resolved =
            resolve_record_target(&targets, CaptureSourceKind::Display, None, Some(99)).unwrap();

        assert_eq!(resolved.id, 1);
    }

    #[test]
    fn no_selector_without_targets_of_kind_fails() {
        let targets = [window(1, "Notes")];

        let error = resolve(&targets, None).unwrap_err();

        assert_eq!(error.code, "target_not_found");
    }

    #[test]
    fn settings_source_follows_resolved_target() {
        let target = CaptureTarget {
            id: 42,
            name: "Notes - Draft".into(),
            kind: CaptureSourceKind::Window,
        };
        let settings = settings_for_target(RecorderSettings::default(), &target);

        assert_eq!(settings.source, CaptureSourceKind::Window);
    }

    #[test]
    fn selected_resolution_survives_target_resolution() {
        let target = CaptureTarget {
            id: 1,
            name: "Display".into(),
            kind: CaptureSourceKind::Display,
        };
        let settings = RecorderSettings {
            resolution: Resolution::R720p,
            ..RecorderSettings::default()
        };

        assert_eq!(
            settings_for_target(settings, &target).resolution,
            Resolution::R720p
        );
    }
}
