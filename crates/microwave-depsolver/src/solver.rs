use crate::version::{Constraint, Version};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::time::{Duration, Instant};

/// Upstream's default depsolver timeout.
pub const DEFAULT_TIMEOUT_MS: u64 = 2000;

const MAX_ERROR_PATHS: usize = 100;

/// Options controlling a solve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SolveOptions {
    pub timeout: Duration,
}

impl Default for SolveOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_millis(DEFAULT_TIMEOUT_MS),
        }
    }
}

impl SolveOptions {
    pub fn with_timeout_ms(timeout_ms: u64) -> Self {
        Self {
            timeout: Duration::from_millis(timeout_ms),
        }
    }
}

/// One version of a cookbook and its dependency constraints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageVersion {
    pub version: Version,
    pub dependencies: Vec<(String, Constraint)>,
}

/// All cookbook versions known to an organization.
#[derive(Debug, Clone, Default)]
pub struct World {
    packages: BTreeMap<String, BTreeMap<Version, PackageVersion>>,
}

impl World {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a cookbook version. Adding the same name/version twice replaces the
    /// previous dependency metadata.
    pub fn insert<I>(&mut self, name: impl Into<String>, version: Version, dependencies: I)
    where
        I: IntoIterator<Item = (String, Constraint)>,
    {
        self.packages.entry(name.into()).or_default().insert(
            version,
            PackageVersion {
                version,
                dependencies: dependencies.into_iter().collect(),
            },
        );
    }

    pub fn contains(&self, name: &str) -> bool {
        self.packages.contains_key(name)
    }

    /// Solve a set of run-list goals against this world.
    ///
    /// Versions excluded by `environment_constraints` are removed before the
    /// search, matching upstream's `find_solution/2` behavior. The solution is
    /// sorted by cookbook name so callers get deterministic output.
    pub fn solve(
        &self,
        goals: &[Goal],
        environment_constraints: &BTreeMap<String, Constraint>,
        options: &SolveOptions,
    ) -> Result<Solution, SolveError> {
        if goals.is_empty() {
            return Ok(Solution::default());
        }

        let mut non_existent_cookbooks = Vec::new();
        for goal in goals {
            if !self.packages.contains_key(&goal.name)
                && !non_existent_cookbooks.contains(&goal.name)
            {
                non_existent_cookbooks.push(goal.name.clone());
            }
        }
        if !non_existent_cookbooks.is_empty() {
            return Err(SolveError::InvalidConstraints(InvalidConstraints {
                non_existent_cookbooks,
                cookbooks_with_no_versions: Vec::new(),
            }));
        }

        let mut cookbooks_with_no_versions = Vec::new();
        for goal in goals {
            let has_match = self.packages.get(&goal.name).is_some_and(|versions| {
                versions.values().any(|package| {
                    goal.constraint.matches(&package.version)
                        && environment_matches(
                            environment_constraints,
                            &goal.name,
                            &package.version,
                        )
                })
            });
            if !has_match {
                let rendered = goal.constraint.format_for(&goal.name);
                if !cookbooks_with_no_versions.contains(&rendered) {
                    cookbooks_with_no_versions.push(rendered);
                }
            }
        }
        if !cookbooks_with_no_versions.is_empty() {
            return Err(SolveError::InvalidConstraints(InvalidConstraints {
                non_existent_cookbooks: Vec::new(),
                cookbooks_with_no_versions,
            }));
        }

        let deadline = Instant::now() + options.timeout;
        let search = Search {
            world: self,
            environment_constraints,
            goals,
            deadline,
        };
        let mut state = State::default();
        for goal in goals {
            state
                .constraints
                .entry(goal.name.clone())
                .or_default()
                .push(goal.constraint.clone());
        }

        match search.search(state) {
            Ok(state) => {
                let packages = state
                    .selected
                    .into_iter()
                    .map(|(name, version)| ResolvedPackage { name, version })
                    .collect();
                Ok(Solution { packages })
            }
            Err(SearchFailure::Timeout) => Err(SolveError::Timeout),
            Err(SearchFailure::Conflict(failure)) => {
                Err(SolveError::NoSolution(search.explain(failure)))
            }
        }
    }

    fn package_versions(&self, name: &str) -> Option<&BTreeMap<Version, PackageVersion>> {
        self.packages.get(name)
    }

    fn dependencies(&self, name: &str, version: &Version) -> &[(String, Constraint)] {
        self.packages
            .get(name)
            .and_then(|versions| versions.get(version))
            .map(|package| package.dependencies.as_slice())
            .unwrap_or(&[])
    }
}

/// A run-list goal: a cookbook name and the constraint it must satisfy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Goal {
    pub name: String,
    pub constraint: Constraint,
}

impl Goal {
    pub fn any(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            constraint: Constraint::any(),
        }
    }

    pub fn exact(name: impl Into<String>, version: Version) -> Self {
        Self {
            name: name.into(),
            constraint: Constraint::exact(version),
        }
    }
}

/// A selected cookbook version.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ResolvedPackage {
    pub name: String,
    pub version: Version,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Solution {
    /// Sorted by cookbook name for deterministic output.
    pub packages: Vec<ResolvedPackage>,
}

impl Solution {
    pub fn get(&self, name: &str) -> Option<Version> {
        self.packages
            .iter()
            .find(|package| package.name == name)
            .map(|package| package.version)
    }
}

/// Failure details for a run list that cannot be validated at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidConstraints {
    pub non_existent_cookbooks: Vec<String>,
    /// Formatted constraints such as `(foo = 2.0.0)`.
    pub cookbooks_with_no_versions: Vec<String>,
}

impl InvalidConstraints {
    pub fn message(&self) -> String {
        let mut parts = Vec::new();
        if !self.non_existent_cookbooks.is_empty() {
            let noun = if self.non_existent_cookbooks.len() == 1 {
                "cookbook"
            } else {
                "cookbooks"
            };
            parts.push(format!(
                "no such {noun} {}",
                self.non_existent_cookbooks.join(", ")
            ));
        }
        if !self.cookbooks_with_no_versions.is_empty() {
            let noun = if self.cookbooks_with_no_versions.len() == 1 {
                "cookbook"
            } else {
                "cookbooks"
            };
            parts.push(format!(
                "no versions match the constraints on {noun} {}",
                self.cookbooks_with_no_versions.join(", ")
            ));
        }

        if parts.is_empty() {
            "Run list contains invalid items.".to_string()
        } else {
            format!("Run list contains invalid items: {}.", parts.join("; "))
        }
    }
}

/// Failure details for a world that has no solution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoSolution {
    pub message: String,
    pub unsatisfiable_run_list_item: String,
    pub non_existent_cookbooks: Vec<String>,
    pub most_constrained_cookbooks: Vec<String>,
}

impl NoSolution {
    /// Upstream reports these failures as an unsatisfiable run list item. This
    /// helper exists for callers that want a boolean instead of inspecting the
    /// message.
    pub fn unable_to_solve(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SolveError {
    InvalidConstraints(InvalidConstraints),
    NoSolution(NoSolution),
    Timeout,
}

impl fmt::Display for SolveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConstraints(error) => formatter.write_str(&error.message()),
            Self::NoSolution(error) => formatter.write_str(&error.message),
            Self::Timeout => formatter.write_str("unable to solve dependencies in allotted time"),
        }
    }
}

impl std::error::Error for SolveError {}

#[derive(Debug, Clone, Default)]
struct State {
    selected: BTreeMap<String, Version>,
    constraints: BTreeMap<String, Vec<Constraint>>,
}

#[derive(Debug)]
struct Failure {
    package: String,
    /// Candidate versions that were attempted, in the order they were tried.
    tried: Vec<Version>,
}

#[derive(Debug)]
enum SearchFailure {
    Conflict(Failure),
    Timeout,
}

struct Search<'a> {
    world: &'a World,
    environment_constraints: &'a BTreeMap<String, Constraint>,
    goals: &'a [Goal],
    deadline: Instant,
}

impl Search<'_> {
    fn search(&self, state: State) -> Result<State, SearchFailure> {
        self.check_timeout()?;

        let Some(name) = self.next_package(&state) else {
            return Ok(state);
        };
        let candidates = self.candidates(&name, &state);
        if candidates.is_empty() {
            return Err(SearchFailure::Conflict(Failure {
                package: name,
                tried: Vec::new(),
            }));
        }

        let mut first_failure = None;
        for version in candidates {
            self.check_timeout()?;
            let mut next = state.clone();
            next.selected.insert(name.clone(), version);

            let mut conflict = None;
            for (dependency_name, dependency_constraint) in self.world.dependencies(&name, &version)
            {
                next.constraints
                    .entry(dependency_name.clone())
                    .or_default()
                    .push(dependency_constraint.clone());
                if let Some(selected) = next.selected.get(dependency_name) {
                    if !self.version_matches(dependency_name, selected, &next) {
                        conflict = Some(Failure {
                            package: dependency_name.clone(),
                            tried: vec![*selected],
                        });
                        break;
                    }
                }
            }

            if conflict.is_none() {
                match self.search(next) {
                    Ok(solution) => return Ok(solution),
                    Err(SearchFailure::Timeout) => return Err(SearchFailure::Timeout),
                    Err(SearchFailure::Conflict(failure)) => conflict = Some(failure),
                }
            }

            if first_failure.is_none() {
                first_failure = conflict;
            }
        }

        Err(SearchFailure::Conflict(first_failure.unwrap_or(Failure {
            package: name,
            tried: Vec::new(),
        })))
    }

    fn check_timeout(&self) -> Result<(), SearchFailure> {
        if Instant::now() >= self.deadline {
            Err(SearchFailure::Timeout)
        } else {
            Ok(())
        }
    }

    /// Pick the unselected package with the fewest admissible versions, then
    /// break ties by name. This keeps the search deterministic and fails fast
    /// on over-constrained packages.
    fn next_package(&self, state: &State) -> Option<String> {
        state
            .constraints
            .keys()
            .filter(|name| !state.selected.contains_key(*name))
            .min_by(|left, right| {
                self.candidate_count(left, state)
                    .cmp(&self.candidate_count(right, state))
                    .then_with(|| left.cmp(right))
            })
            .cloned()
    }

    fn candidate_count(&self, name: &str, state: &State) -> usize {
        self.candidates(name, state).len()
    }

    fn candidates(&self, name: &str, state: &State) -> Vec<Version> {
        self.world
            .package_versions(name)
            .map(|versions| {
                versions
                    .values()
                    .rev()
                    .filter(|package| self.version_matches(name, &package.version, state))
                    .map(|package| package.version)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn version_matches(&self, name: &str, version: &Version, state: &State) -> bool {
        if !environment_matches(self.environment_constraints, name, version) {
            return false;
        }
        state
            .constraints
            .get(name)
            .is_none_or(|constraints| constraints.iter().all(|item| item.matches(version)))
    }

    fn explain(&self, failure: Failure) -> NoSolution {
        let exists = self.world.contains(&failure.package);
        let culprit_version = failure.tried.last().copied().or_else(|| {
            self.world
                .package_versions(&failure.package)
                .and_then(|versions| {
                    versions
                        .values()
                        .rev()
                        .find(|package| {
                            environment_matches(
                                self.environment_constraints,
                                &failure.package,
                                &package.version,
                            )
                        })
                        .map(|package| package.version)
                })
        });

        let dependencies = culprit_version
            .map(|version| self.world.dependencies(&failure.package, &version))
            .unwrap_or(&[]);
        let dependency_text = dependencies
            .iter()
            .map(|(name, constraint)| constraint.format_for(name))
            .collect::<Vec<_>>()
            .join(", ");

        let most_constrained_cookbooks = if exists {
            culprit_version
                .map(|version| {
                    vec![format!(
                        "{} = {} -> [{dependency_text}]",
                        failure.package, version
                    )]
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        let non_existent_cookbooks = if exists {
            Vec::new()
        } else {
            vec![failure.package.clone()]
        };

        let goal = self.unsatisfiable_goal(&failure.package);
        let unsatisfiable_run_list_item = goal.constraint.format_for(&goal.name);
        let paths = self.dependency_paths(&failure.package);
        let paths = if paths.is_empty() {
            "[]".to_string()
        } else {
            paths
                .iter()
                .map(|path| format!("[{path}]"))
                .collect::<Vec<_>>()
                .join(", ")
        };

        let existence = if exists {
            String::new()
        } else {
            ", which does not exist,".to_string()
        };
        let message = format!(
            "Unable to satisfy constraints on package {}{existence} due to solution constraint {}. \
             Solution constraints that may result in a constraint on {}: {paths}",
            failure.package, unsatisfiable_run_list_item, failure.package
        );

        NoSolution {
            message,
            unsatisfiable_run_list_item,
            non_existent_cookbooks,
            most_constrained_cookbooks,
        }
    }

    /// Find the first run-list item whose transitive dependencies can reach
    /// `package`. Upstream reports the run-list constraint that started the
    /// failing chain.
    fn unsatisfiable_goal(&self, package: &str) -> &Goal {
        for goal in self.goals {
            if goal.name == package || self.goal_reaches(&goal.name, package) {
                return goal;
            }
        }
        &self.goals[0]
    }

    fn goal_reaches(&self, root: &str, target: &str) -> bool {
        let mut pending = vec![root.to_string()];
        let mut visited = BTreeSet::new();
        while let Some(name) = pending.pop() {
            if name == target {
                return true;
            }
            if !visited.insert(name.clone()) {
                continue;
            }
            if let Some(versions) = self.world.package_versions(&name) {
                for package in versions.values() {
                    for (dependency, _) in &package.dependencies {
                        pending.push(dependency.clone());
                    }
                }
            }
        }
        false
    }

    /// Render dependency chains that can put a constraint on `culprit`. Each
    /// path starts at a run-list goal and follows dependency edges.
    fn dependency_paths(&self, culprit: &str) -> Vec<String> {
        let mut paths = Vec::new();
        for goal in self.goals {
            let Some(versions) = self.world.package_versions(&goal.name) else {
                continue;
            };
            for package in versions.values() {
                if !goal.constraint.matches(&package.version)
                    || !environment_matches(
                        self.environment_constraints,
                        &goal.name,
                        &package.version,
                    )
                {
                    continue;
                }

                let mut constraints = BTreeMap::new();
                constraints
                    .entry(goal.name.clone())
                    .or_insert_with(Vec::new)
                    .push(goal.constraint.clone());
                let mut visited = BTreeSet::new();
                visited.insert(goal.name.clone());
                let mut edges = Vec::new();
                self.collect_paths(
                    &goal.name,
                    package.version,
                    culprit,
                    &constraints,
                    &mut visited,
                    &mut edges,
                    &mut paths,
                );
                if paths.len() >= MAX_ERROR_PATHS {
                    break;
                }
            }
            if paths.len() >= MAX_ERROR_PATHS {
                break;
            }
        }

        let mut seen = BTreeSet::new();
        paths
            .into_iter()
            .filter(|path| seen.insert(path.clone()))
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_paths(
        &self,
        parent: &str,
        parent_version: Version,
        culprit: &str,
        constraints: &BTreeMap<String, Vec<Constraint>>,
        visited: &mut BTreeSet<String>,
        edges: &mut Vec<PathEdge>,
        paths: &mut Vec<String>,
    ) {
        if paths.len() >= MAX_ERROR_PATHS {
            return;
        }

        for (child, constraint) in self.world.dependencies(parent, &parent_version) {
            if paths.len() >= MAX_ERROR_PATHS {
                return;
            }
            edges.push(PathEdge {
                parent: parent.to_string(),
                parent_version,
                child: child.clone(),
                constraint: constraint.clone(),
            });

            if child == culprit {
                paths.push(render_path(edges));
            } else if !visited.contains(child) {
                let mut child_constraints = constraints.clone();
                child_constraints
                    .entry(child.clone())
                    .or_default()
                    .push(constraint.clone());
                if let Some(versions) = self.world.package_versions(child) {
                    visited.insert(child.clone());
                    for package in versions.values() {
                        let matches_constraints =
                            child_constraints.get(child).is_none_or(|items| {
                                items.iter().all(|item| item.matches(&package.version))
                            });
                        if matches_constraints
                            && environment_matches(
                                self.environment_constraints,
                                child,
                                &package.version,
                            )
                        {
                            self.collect_paths(
                                child,
                                package.version,
                                culprit,
                                &child_constraints,
                                visited,
                                edges,
                                paths,
                            );
                        }
                        if paths.len() >= MAX_ERROR_PATHS {
                            break;
                        }
                    }
                    visited.remove(child);
                }
            }

            edges.pop();
        }
    }
}

#[derive(Debug)]
struct PathEdge {
    parent: String,
    parent_version: Version,
    child: String,
    constraint: Constraint,
}

fn render_path(edges: &[PathEdge]) -> String {
    let mut rendered = String::new();
    for (index, edge) in edges.iter().enumerate() {
        if index > 0 {
            rendered.push_str(" -> ");
        }
        if index == 0 {
            rendered.push_str(&format!("({} = {}) -> ", edge.parent, edge.parent_version));
        }
        rendered.push_str(&edge.constraint.format_for(&edge.child));
    }
    rendered
}

fn environment_matches(
    environment_constraints: &BTreeMap<String, Constraint>,
    name: &str,
    version: &Version,
) -> bool {
    environment_constraints
        .get(name)
        .is_none_or(|constraint| constraint.matches(version))
}
