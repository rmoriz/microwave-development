use microwave_depsolver::{Constraint, Goal, NoSolution, SolveError, SolveOptions, Version, World};
use std::collections::BTreeMap;
use std::time::Duration;

fn constraint(input: &str) -> Constraint {
    Constraint::parse(input).expect("test constraint should parse")
}

fn version(input: &str) -> Version {
    Version::parse(input).expect("test version should parse")
}

type PackageSpec<'a> = (&'a str, &'a str, &'a [(&'a str, &'a str)]);

fn world(specification: &[PackageSpec<'_>]) -> World {
    let mut world = World::new();
    for (name, package_version, dependencies) in specification {
        let dependencies = dependencies
            .iter()
            .map(|(dependency, item)| ((*dependency).to_string(), constraint(item)))
            .collect::<Vec<_>>();
        world.insert(*name, version(package_version), dependencies);
    }
    world
}

fn solve(
    world: &World,
    goals: &[(&str, Option<&str>)],
) -> Result<microwave_depsolver::Solution, SolveError> {
    let goals = goals
        .iter()
        .map(|(name, package_version)| match package_version {
            Some(package_version) => Goal::exact(*name, version(package_version)),
            None => Goal::any(*name),
        })
        .collect::<Vec<_>>();
    world.solve(&goals, &BTreeMap::new(), &SolveOptions::default())
}

fn selected(result: Result<microwave_depsolver::Solution, SolveError>) -> Vec<(String, String)> {
    result
        .expect("expected a solution")
        .packages
        .into_iter()
        .map(|package| (package.name, package.version.to_string()))
        .collect()
}

fn no_solution(result: Result<microwave_depsolver::Solution, SolveError>) -> NoSolution {
    match result {
        Err(SolveError::NoSolution(error)) => error,
        other => panic!("expected no_solution, got {other:?}"),
    }
}

fn invalid(
    result: Result<microwave_depsolver::Solution, SolveError>,
) -> (Vec<String>, Vec<String>) {
    match result {
        Err(SolveError::InvalidConstraints(error)) => (
            error.non_existent_cookbooks,
            error.cookbooks_with_no_versions,
        ),
        other => panic!("expected invalid_constraints, got {other:?}"),
    }
}

fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut pairs = items
        .iter()
        .map(|(name, version)| ((*name).to_string(), (*version).to_string()))
        .collect::<Vec<_>>();
    pairs.sort();
    pairs
}

#[test]
fn parses_two_and_three_component_versions() {
    assert_eq!(version("1.2"), Version::new(1, 2, 0));
    assert_eq!(version("1.2.3"), Version::new(1, 2, 3));
    assert_eq!(version("1").to_string(), "1.0.0");
    assert_eq!(version("1.2").to_string(), "1.2.0");
}

#[test]
fn rejects_invalid_versions() {
    for input in ["", "1.2.3.4", "1.-2.3", "-1.2.3", "1..3", "1.2.x", "1.2.-1"] {
        assert!(Version::parse(input).is_err(), "{input} should be invalid");
    }
}

#[test]
fn parses_all_supported_constraint_operators() {
    for (input, symbol) in [
        ("= 1.2.3", "="),
        ("1.2.3", "="),
        ("> 1.2.3", ">"),
        ("< 1.2.3", "<"),
        (">= 1.2.3", ">="),
        ("<= 1.2.3", "<="),
        ("~> 1.2", "~>"),
        (">=1.2.3", ">="),
    ] {
        assert_eq!(constraint(input).operator.symbol(), symbol, "{input}");
    }
}

#[test]
fn pessimistic_constraint_semantics() {
    let major_minor = constraint("~> 2.1");
    assert!(major_minor.matches(&version("2.1.0")));
    assert!(major_minor.matches(&version("2.2.0")));
    assert!(major_minor.matches(&version("2.99.99")));
    assert!(!major_minor.matches(&version("3.0.0")));
    assert!(!major_minor.matches(&version("2.0.9")));

    let major_minor_patch = constraint("~> 2.1.1");
    assert!(major_minor_patch.matches(&version("2.1.1")));
    assert!(major_minor_patch.matches(&version("2.1.5")));
    assert!(!major_minor_patch.matches(&version("2.2.0")));
    assert!(!major_minor_patch.matches(&version("2.1.0")));
}

/// Port of `depsolver_tests:filter_versions/0`, using one constraint per
/// cookbook and `>=`/`<=` in place of upstream's `between` operator.
#[test]
fn filters_versions_like_upstream_depsolver_test() {
    let packages = [
        ("app1", "0.1.0"),
        ("app1", "0.2"),
        ("app1", "0.2"),
        ("app1", "3.0"),
        ("app2", "0.0.1"),
        ("app2", "0.1"),
        ("app2", "1.0"),
        ("app2", "2.1.5"),
        ("app2", "2.2"),
        ("app2", "3.0"),
        ("app3", "0.1.0"),
        ("app3", "0.1.3"),
        ("app3", "2.0.0"),
        ("app3", "3.0.0"),
        ("app3", "4.0.0"),
        ("app4", "0.1.0"),
        ("app4", "0.3.0"),
        ("app4", "5.0.0"),
        ("app4", "6.0.0"),
        ("app5", "0.1.0"),
        ("app5", "0.3.0"),
        ("app5", "2.0.0"),
        ("app5", "6.0.0"),
    ];
    let constraints = BTreeMap::from([
        ("app2".to_string(), vec![constraint("~> 2.1")]),
        (
            "app3".to_string(),
            vec![constraint(">= 0.1.1"), constraint("<= 0.1.5")],
        ),
        ("app4".to_string(), vec![constraint(">= 5.0.0")]),
        ("app5".to_string(), vec![constraint(">= 2.0.0")]),
    ]);

    let filtered = packages
        .iter()
        .filter(|(name, package_version)| {
            constraints.get(*name).is_none_or(|items| {
                items
                    .iter()
                    .all(|item| item.matches(&version(package_version)))
            })
        })
        .copied()
        .collect::<Vec<_>>();

    assert_eq!(
        filtered,
        vec![
            ("app1", "0.1.0"),
            ("app1", "0.2"),
            ("app1", "0.2"),
            ("app1", "3.0"),
            ("app2", "2.1.5"),
            ("app2", "2.2"),
            ("app3", "0.1.3"),
            ("app4", "5.0.0"),
            ("app4", "6.0.0"),
            ("app5", "2.0.0"),
            ("app5", "6.0.0"),
        ]
    );
}

#[test]
fn solves_empty_run_list() {
    let world = world(&[("foo", "1.2.3", &[])]);
    assert_eq!(selected(solve(&world, &[])), Vec::<(String, String)>::new());
}

#[test]
fn reports_empty_world_as_non_existent() {
    let world = World::new();
    assert_eq!(
        invalid(solve(&world, &[("foo", None)])),
        (vec!["foo".to_string()], vec![])
    );
}

#[test]
fn reports_missing_exact_version_as_constraint_not_met() {
    let world = world(&[("foo", "1.2.3", &[])]);
    assert_eq!(
        invalid(solve(&world, &[("foo", Some("2.0.0"))])),
        (vec![], vec!["(foo = 2.0.0)".to_string()])
    );
}

#[test]
fn reports_missing_dependency() {
    let world = world(&[("foo", "1.2.3", &[("bar", "> 2.0.0")])]);
    let error = no_solution(solve(&world, &[("foo", None)]));

    assert_eq!(
        error,
        NoSolution {
            message: "Unable to satisfy constraints on package bar, which does not exist, due to solution constraint (foo >= 0.0.0). Solution constraints that may result in a constraint on bar: [(foo = 1.2.3) -> (bar > 2.0.0)]".to_string(),
            unsatisfiable_run_list_item: "(foo >= 0.0.0)".to_string(),
            non_existent_cookbooks: vec!["bar".to_string()],
            most_constrained_cookbooks: vec![],
        }
    );
}

#[test]
fn reports_dependency_that_is_not_new_enough() {
    let world = world(&[
        ("foo", "1.2.3", &[("bar", "> 2.0.0")]),
        ("bar", "2.0.0", &[]),
    ]);
    let mut environment = BTreeMap::new();
    environment.insert("foo".to_string(), constraint("= 1.2.3"));
    let result = world
        .solve(&[Goal::any("foo")], &environment, &SolveOptions::default())
        .unwrap_err();

    let SolveError::NoSolution(error) = result else {
        panic!("expected no_solution");
    };
    assert_eq!(
        error.message,
        "Unable to satisfy constraints on package bar due to solution constraint (foo >= 0.0.0). \
         Solution constraints that may result in a constraint on bar: [(foo = 1.2.3) -> (bar > 2.0.0)]"
    );
    assert_eq!(
        error.unsatisfiable_run_list_item,
        "(foo >= 0.0.0)".to_string()
    );
    assert_eq!(error.non_existent_cookbooks, Vec::<String>::new());
    assert_eq!(
        error.most_constrained_cookbooks,
        vec!["bar = 2.0.0 -> []".to_string()]
    );
    assert!(error.unable_to_solve());
}

#[test]
fn reports_impossible_circular_dependency() {
    let world = world(&[
        ("foo", "1.2.3", &[("bar", "> 2.0.0")]),
        ("bar", "2.0.0", &[("foo", "> 3.0.0")]),
    ]);
    let error = no_solution(solve(&world, &[("foo", None)]));

    assert_eq!(
        error.message,
        "Unable to satisfy constraints on package bar due to solution constraint (foo >= 0.0.0). \
         Solution constraints that may result in a constraint on bar: [(foo = 1.2.3) -> (bar > 2.0.0)]"
    );
    assert_eq!(
        error.most_constrained_cookbooks,
        vec!["bar = 2.0.0 -> [(foo > 3.0.0)]".to_string()]
    );
}

#[test]
fn respects_environment_constraints() {
    let world = world(&[
        ("foo", "1.2.3", &[("bar", "> 2.0.0")]),
        ("foo", "1.0.0", &[("bar", "= 1.0.0")]),
        ("bar", "1.0.0", &[]),
        ("bar", "3.0.0", &[]),
    ]);

    let mut environment = BTreeMap::new();
    environment.insert("bar".to_string(), constraint("= 1.0.0"));
    assert_eq!(
        selected(Ok(world
            .solve(&[Goal::any("foo")], &environment, &SolveOptions::default())
            .expect("environment should select foo 1.0.0"))),
        pairs(&[("bar", "1.0.0"), ("foo", "1.0.0")])
    );

    let mut environment = BTreeMap::new();
    environment.insert("bar".to_string(), constraint("> 1.1.0"));
    assert_eq!(
        selected(Ok(world
            .solve(&[Goal::any("foo")], &environment, &SolveOptions::default())
            .expect("environment should select foo 1.2.3"))),
        pairs(&[("bar", "3.0.0"), ("foo", "1.2.3")])
    );
}

#[test]
fn reports_impossible_dependency_via_environment() {
    let world = world(&[
        ("foo", "1.2.3", &[("bar", "> 2.0.0")]),
        ("bar", "1.0.0", &[]),
        ("bar", "3.0.0", &[]),
    ]);
    let mut environment = BTreeMap::new();
    environment.insert("bar".to_string(), constraint("= 1.0.0"));

    assert!(world
        .solve(
            &[Goal::any("foo")],
            &BTreeMap::new(),
            &SolveOptions::default()
        )
        .is_ok());

    let error = match world.solve(&[Goal::any("foo")], &environment, &SolveOptions::default()) {
        Err(SolveError::NoSolution(error)) => error,
        other => panic!("expected no_solution, got {other:?}"),
    };
    assert_eq!(
        error.most_constrained_cookbooks,
        vec!["bar = 1.0.0 -> []".to_string()]
    );
}

#[test]
fn reports_complex_conflict_with_all_blame_paths() {
    let world = world(&[
        ("foo", "1.2.3", &[("bar", "= 1.0.0"), ("buzz", "= 1.0.0")]),
        ("bar", "1.0.0", &[("baz", "= 1.0.0")]),
        ("buzz", "1.0.0", &[("baz", "> 1.2.0")]),
        ("buzz", "2.0.0", &[("baz", "= 1.0.0")]),
        ("ack", "1.0.0", &[("foobar", "= 1.0.0")]),
        ("baz", "1.0.0", &[]),
        ("baz", "2.0.0", &[]),
    ]);

    let result = solve(&world, &[("foo", None), ("buzz", None)]);
    let error = no_solution(result);

    assert_eq!(
        error.message,
        "Unable to satisfy constraints on package baz due to solution constraint (foo >= 0.0.0). \
         Solution constraints that may result in a constraint on baz: \
         [(foo = 1.2.3) -> (bar = 1.0.0) -> (baz = 1.0.0)], \
         [(foo = 1.2.3) -> (buzz = 1.0.0) -> (baz > 1.2.0)], \
         [(buzz = 1.0.0) -> (baz > 1.2.0)], \
         [(buzz = 2.0.0) -> (baz = 1.0.0)]"
    );
    assert_eq!(
        error.most_constrained_cookbooks,
        vec!["baz = 1.0.0 -> []".to_string()]
    );
}

#[test]
fn solves_first_upstream_depsolver_case() {
    let world = world(&[
        ("app1", "0.1", &[("app2", "= 0.2.33"), ("app3", ">= 0.2")]),
        ("app1", "0.2", &[]),
        ("app1", "0.3", &[]),
        ("app2", "0.1", &[]),
        ("app2", "0.2.33", &[("app3", "= 0.3")]),
        ("app2", "0.3", &[]),
        ("app3", "0.1", &[]),
        ("app3", "0.2", &[]),
        ("app3", "0.3", &[]),
    ]);

    assert_eq!(
        selected(solve(&world, &[("app1", Some("0.1"))])),
        pairs(&[("app1", "0.1.0"), ("app2", "0.2.33"), ("app3", "0.3.0")])
    );
}

#[test]
fn solves_second_upstream_depsolver_case() {
    let world = world(&[
        (
            "app1",
            "0.1",
            &[("app2", ">= 0.1"), ("app4", "= 0.2"), ("app3", ">= 0.2")],
        ),
        ("app1", "0.2", &[]),
        ("app1", "0.3", &[]),
        ("app2", "0.1", &[("app3", ">= 0.2")]),
        ("app2", "0.2", &[("app3", ">= 0.2")]),
        ("app2", "0.3", &[("app3", ">= 0.2")]),
        ("app3", "0.1", &[("app4", ">= 0.2")]),
        ("app3", "0.2", &[("app4", "= 0.2")]),
        ("app3", "0.3", &[]),
        ("app4", "0.1", &[]),
        ("app4", "0.2", &[("app2", ">= 0.2"), ("app3", "= 0.3")]),
        ("app4", "0.3", &[]),
    ]);

    assert_eq!(
        selected(solve(
            &world,
            &[("app1", Some("0.1")), ("app2", Some("0.3"))]
        )),
        pairs(&[
            ("app1", "0.1.0"),
            ("app2", "0.3.0"),
            ("app3", "0.3.0"),
            ("app4", "0.2.0"),
        ])
    );
}

#[test]
fn solves_third_upstream_depsolver_case() {
    let world = world(&[
        (
            "app1",
            "0.1.0",
            &[
                ("app2", ">= 0.1.0"),
                ("app3", ">= 0.1.1"),
                ("app3", "<= 0.1.5"),
            ],
        ),
        (
            "app1",
            "0.2",
            &[
                ("app2", ">= 0.1.0"),
                ("app3", ">= 0.1.1"),
                ("app3", "<= 0.1.5"),
            ],
        ),
        (
            "app1",
            "3.0",
            &[
                ("app2", ">= 0.1.0"),
                ("app3", ">= 0.1.1"),
                ("app3", "<= 0.1.5"),
            ],
        ),
        ("app2", "0.0.1", &[("app4", ">= 5.0.0")]),
        ("app2", "0.1", &[("app4", ">= 5.0.0")]),
        ("app2", "1.0", &[("app4", ">= 5.0.0")]),
        ("app2", "3.0", &[("app4", ">= 5.0.0")]),
        ("app3", "0.1.0", &[("app5", ">= 2.0.0")]),
        ("app3", "0.1.3", &[("app5", ">= 2.0.0")]),
        ("app3", "2.0.0", &[("app5", ">= 2.0.0")]),
        ("app3", "3.0.0", &[("app5", ">= 2.0.0")]),
        ("app3", "4.0.0", &[("app5", ">= 2.0.0")]),
        ("app4", "0.1.0", &[("app5", ">= 0.0.0")]),
        ("app4", "0.3.0", &[("app5", ">= 0.0.0")]),
        ("app4", "5.0.0", &[("app5", ">= 0.0.0")]),
        ("app4", "6.0.0", &[("app5", ">= 0.0.0")]),
        ("app5", "0.1.0", &[]),
        ("app5", "0.3.0", &[]),
        ("app5", "2.0.0", &[]),
        ("app5", "6.0.0", &[]),
    ]);

    let expected = pairs(&[
        ("app1", "3.0.0"),
        ("app2", "3.0.0"),
        ("app3", "0.1.3"),
        ("app4", "6.0.0"),
        ("app5", "6.0.0"),
    ]);
    assert_eq!(selected(solve(&world, &[("app1", Some("3.0"))])), expected);
    assert_eq!(selected(solve(&world, &[("app1", None)])), expected);
}

#[test]
fn reports_conflicting_dependencies() {
    let world = world(&[
        ("app1", "0.1", &[("app2", "= 0.2"), ("app3", ">= 0.2")]),
        ("app1", "0.2", &[]),
        ("app1", "0.3", &[]),
        ("app2", "0.1", &[]),
        ("app2", "0.2", &[("app3", "= 0.1")]),
        ("app2", "0.3", &[]),
        ("app3", "0.1", &[]),
        ("app3", "0.2", &[]),
        ("app3", "0.3", &[]),
    ]);

    assert!(matches!(
        solve(&world, &[("app1", Some("0.1"))]),
        Err(SolveError::NoSolution(_))
    ));
}

#[test]
fn solves_conflicting_constraints_when_intersection_exists() {
    let world = world(&[
        (
            "app1",
            "3.0",
            &[
                ("app2", ">= 0.1.0"),
                ("app5", "= 2.0.0"),
                ("app4", ">= 0.3.0"),
                ("app4", "<= 5.0.0"),
                ("app3", ">= 0.1.1"),
                ("app3", "<= 0.1.5"),
            ],
        ),
        ("app2", "3.0", &[("app4", ">= 3.0.0")]),
        ("app3", "0.1.3", &[("app5", ">= 2.0.0")]),
        ("app4", "0.3.0", &[("app5", "= 0.3.0")]),
        ("app4", "5.0.0", &[("app5", "= 2.0.0")]),
        ("app4", "6.0.0", &[("app5", "= 6.0.0")]),
        ("app5", "0.1.0", &[]),
        ("app5", "0.3.0", &[]),
        ("app5", "2.0.0", &[]),
        ("app5", "6.0.0", &[]),
    ]);

    assert_eq!(
        selected(solve(&world, &[("app1", Some("3.0"))])),
        pairs(&[
            ("app1", "3.0.0"),
            ("app2", "3.0.0"),
            ("app3", "0.1.3"),
            ("app4", "5.0.0"),
            ("app5", "2.0.0"),
        ])
    );
}

#[test]
fn solves_with_valid_cycles() {
    let world = world(&[
        ("app1", "0.1.0", &[("app2", ">= 0.0.0")]),
        ("app2", "0.0.1", &[("app1", ">= 0.0.0")]),
    ]);

    assert_eq!(
        selected(solve(&world, &[("app1", Some("0.1.0"))])),
        pairs(&[("app1", "0.1.0"), ("app2", "0.0.1")])
    );
}

#[test]
fn reports_conflicting_dependencies_that_have_no_intersection() {
    let world = world(&[
        (
            "app1",
            "3.0",
            &[
                ("app2", ">= 0.0.0"),
                ("app5", "= 2.0.0"),
                ("app4", ">= 0.3.0"),
                ("app4", "<= 5.0.0"),
            ],
        ),
        ("app2", "0.0.1", &[("app4", ">= 5.0.0")]),
        ("app3", "0.1.0", &[("app5", "= 6.0.0")]),
        ("app4", "5.0.0", &[("app5", "= 2.0.0")]),
        ("app5", "2.0.0", &[]),
        ("app5", "6.0.0", &[]),
    ]);

    assert!(matches!(
        solve(&world, &[("app1", None), ("app3", None)]),
        Err(SolveError::NoSolution(_))
    ));
}

#[test]
fn pessimistic_major_minor_patch_selection() {
    let world = world(&[
        (
            "app1",
            "3.0",
            &[
                ("app2", "~> 2.1.1"),
                ("app3", ">= 0.1.1"),
                ("app3", "<= 0.1.5"),
            ],
        ),
        ("app2", "2.1.5", &[("app4", ">= 5.0.0")]),
        ("app2", "2.2", &[("app4", ">= 5.0.0")]),
        ("app2", "3.0", &[("app4", ">= 5.0.0")]),
        ("app3", "0.1.3", &[("app5", ">= 2.0.0")]),
        ("app4", "5.0.0", &[("app5", ">= 0.0.0")]),
        ("app4", "6.0.0", &[("app5", ">= 0.0.0")]),
        ("app5", "2.0.0", &[]),
        ("app5", "6.0.0", &[]),
    ]);

    assert_eq!(
        selected(solve(&world, &[("app1", Some("3.0"))])),
        pairs(&[
            ("app1", "3.0.0"),
            ("app2", "2.1.5"),
            ("app3", "0.1.3"),
            ("app4", "6.0.0"),
            ("app5", "6.0.0"),
        ])
    );
}

#[test]
fn pessimistic_major_minor_selection() {
    let world = world(&[
        (
            "app1",
            "3.0",
            &[
                ("app2", "~> 2.1"),
                ("app3", ">= 0.1.1"),
                ("app3", "<= 0.1.5"),
            ],
        ),
        ("app2", "2.1.5", &[("app4", ">= 5.0.0")]),
        ("app2", "2.2", &[("app4", ">= 5.0.0")]),
        ("app2", "3.0", &[("app4", ">= 5.0.0")]),
        ("app3", "0.1.3", &[("app5", ">= 2.0.0")]),
        ("app4", "5.0.0", &[("app5", ">= 0.0.0")]),
        ("app4", "6.0.0", &[("app5", ">= 0.0.0")]),
        ("app5", "2.0.0", &[]),
        ("app5", "6.0.0", &[]),
    ]);

    assert_eq!(
        selected(solve(&world, &[("app1", Some("3.0"))])),
        pairs(&[
            ("app1", "3.0.0"),
            ("app2", "2.2.0"),
            ("app3", "0.1.3"),
            ("app4", "6.0.0"),
            ("app5", "6.0.0"),
        ])
    );
}

#[test]
fn reports_missing_cookbook_before_dependency_failure() {
    let world = world(&[
        (
            "app1",
            "0.1",
            &[("app2", "= 0.2"), ("app3", ">= 0.2"), ("app4", "= 0.2")],
        ),
        ("app2", "0.1", &[]),
        ("app2", "0.2", &[("app3", "= 0.3")]),
        ("app2", "0.3", &[]),
        ("app3", "0.1", &[]),
        ("app3", "0.2", &[]),
        ("app3", "0.3", &[]),
    ]);

    assert_eq!(
        invalid(solve(
            &world,
            &[("app4", Some("0.1")), ("app3", Some("0.1"))]
        )),
        (vec!["app4".to_string()], vec![])
    );
    assert!(matches!(
        solve(&world, &[("app1", Some("0.1"))]),
        Err(SolveError::NoSolution(_))
    ));
}

#[test]
fn reports_missing_transitive_cookbook() {
    let world = world(&[
        ("app1", "1.1.0", &[]),
        ("app2", "0.0.1", &[("app1::oops", ">= 0.0.0")]),
    ]);

    let error = no_solution(solve(&world, &[("app1", None), ("app2", None)]));
    assert_eq!(error.non_existent_cookbooks, vec!["app1::oops".to_string()]);
}

#[test]
fn rejects_negative_versions_at_parse_time() {
    assert!(Version::parse("1.1.-42").is_err());
}

#[test]
fn reports_binary_cycle_as_unsolvable() {
    let world = world(&[
        ("foo", "1.2.3", &[("bar", "> 2.0.0")]),
        ("bar", "2.0.0", &[("foo", "> 3.0.0")]),
    ]);

    assert!(matches!(
        solve(&world, &[("foo", None)]),
        Err(SolveError::NoSolution(_))
    ));
}

#[test]
fn times_out_when_deadline_is_zero() {
    let world = world(&[("foo", "1.0.0", &[])]);
    let result = world.solve(
        &[Goal::any("foo")],
        &BTreeMap::new(),
        &SolveOptions {
            timeout: Duration::ZERO,
        },
    );
    assert_eq!(result, Err(SolveError::Timeout));
}

#[test]
fn chooses_versions_numerically_and_deterministically() {
    let world = world(&[
        ("app", "2.0.0", &[]),
        ("app", "10.0.0", &[]),
        ("app", "9.0.0", &[]),
    ]);

    let first = selected(solve(&world, &[("app", None)]));
    let second = selected(solve(&world, &[("app", None)]));
    assert_eq!(first, pairs(&[("app", "10.0.0")]));
    assert_eq!(first, second);
}

#[test]
fn retries_lower_version_after_dependency_conflict() {
    let world = world(&[
        ("app", "2.0.0", &[("lib", "= 2.0.0")]),
        ("app", "1.0.0", &[("lib", "= 1.0.0")]),
        ("lib", "1.0.0", &[]),
        ("lib", "2.0.0", &[]),
        ("other", "1.0.0", &[("lib", "= 1.0.0")]),
    ]);

    assert_eq!(
        selected(solve(&world, &[("app", None), ("other", None)])),
        pairs(&[("app", "1.0.0"), ("lib", "1.0.0"), ("other", "1.0.0"),])
    );
}
