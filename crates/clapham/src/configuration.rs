use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use crate::{
    Bounds, Call, ComposeOptions, ComposedSet, CompositionCode, CompositionError, Contribution,
    Handler, HandlerFuture, OAuthResource, OperationName, ResourcePolicy, compose_with_options,
};

/// A local provider or a host-established remote connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged, deny_unknown_fields)]
pub enum Source {
    /// A provider registered by the application.
    Provider {
        /// Registration name.
        provider: String,
    },
    /// A connection established by the host with audience-appropriate credentials.
    Remote {
        /// Host-approved MCP endpoint.
        endpoint: String,
        /// Host credential reference, not a bearer token.
        credential: String,
    },
}

impl Source {
    /// Reference a registered provider.
    pub fn provider(name: impl Into<String>) -> Self {
        Self::Provider {
            provider: name.into(),
        }
    }
    /// Reference a host-established connection.
    pub fn remote(endpoint: impl Into<String>, credential: impl Into<String>) -> Self {
        Self::Remote {
            endpoint: endpoint.into(),
            credential: credential.into(),
        }
    }
}

/// Additional restrictions attached by composition.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Requirements {
    /// Every listed scope is required in addition to provider requirements.
    pub scopes: Vec<String>,
    /// A registered typed resource policy.
    pub policy: Option<String>,
}

/// Command-line presentation settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct CliSettings {
    /// Require explicit confirmation before a CLI invocation.
    pub confirm: bool,
    /// Positional property names in order, where configured.
    pub positional: Option<Vec<String>>,
}

/// One selected provider operation, optionally exposed under an alias.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Exposure {
    /// Source registration in this configuration.
    pub source: String,
    /// Original provider operation name.
    pub name: String,
    /// Public name, where different from the provider's name.
    #[serde(default)]
    pub expose_as: Option<String>,
    /// Additional permission requirements.
    #[serde(default)]
    pub require: Requirements,
    /// Command-line settings.
    #[serde(default)]
    pub cli: CliSettings,
}

impl Exposure {
    /// Expose a named operation from a source.
    pub fn new(source: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            name: name.into(),
            expose_as: None,
            require: Requirements::default(),
            cli: CliSettings::default(),
        }
    }
    /// Select a typed provider operation without repeating its name.
    pub fn typed<I, O>(source: impl Into<String>, operation: &crate::TypedOperation<I, O>) -> Self {
        Self::new(source, operation.descriptor().name.as_str())
    }
    /// Expose under a different complete name.
    pub fn alias(mut self, name: impl Into<String>) -> Self {
        self.expose_as = Some(name.into());
        self
    }
    /// Add an OAuth scope requirement.
    pub fn require_scope(mut self, scope: impl Into<String>) -> Self {
        self.require.scopes.push(scope.into());
        self
    }
    /// Add a typed resource policy by registration name.
    pub fn policy(mut self, name: impl Into<String>) -> Self {
        self.require.policy = Some(name.into());
        self
    }
    /// Require CLI confirmation.
    pub fn confirm(mut self) -> Self {
        self.cli.confirm = true;
        self
    }
    /// Set positional input property names.
    pub fn positional(mut self, properties: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.cli.positional = Some(properties.into_iter().map(Into::into).collect());
        self
    }
}

/// JSON and Rust configuration share one model and one atomic composition path.
///
/// Executable registrations are supplied by the host and are not serialised.
#[derive(Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Configuration {
    /// Resource-server binding checked by the shared runner.
    pub oauth: OAuthResource,
    /// Named sources; duplicate JSON keys are refused.
    #[serde(default, deserialize_with = "unique_sources")]
    pub sources: BTreeMap<String, Source>,
    /// Exposed operations, with provider requirements preserved.
    #[serde(default)]
    pub operations: Vec<Exposure>,
    #[serde(skip)]
    #[schemars(skip)]
    providers: BTreeMap<String, Contribution>,
    #[serde(skip)]
    #[schemars(skip)]
    connections: BTreeMap<(String, String), Contribution>,
    #[serde(skip)]
    #[schemars(skip)]
    policies: BTreeMap<String, ResourcePolicy>,
    #[serde(skip)]
    #[schemars(skip)]
    errors: Vec<String>,
}

impl std::fmt::Debug for Configuration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Configuration")
            .field("oauth", &self.oauth)
            .field("sources", &self.sources)
            .field("operations", &self.operations)
            .finish_non_exhaustive()
    }
}

impl Configuration {
    /// Begin a programmatic configuration for an OAuth resource server.
    pub fn new(issuer: impl Into<String>, audience: impl Into<String>) -> Self {
        Self {
            oauth: OAuthResource {
                issuer: issuer.into(),
                audience: audience.into(),
            },
            ..Default::default()
        }
    }
    /// Load the same configuration model from declarative JSON.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
    /// Register an executable provider referenced by JSON or Rust configuration.
    pub fn register_provider(mut self, name: impl Into<String>, provider: Contribution) -> Self {
        let name = name.into();
        if self.providers.insert(name.clone(), provider).is_some() {
            self.errors.push(format!("duplicate provider {name}"));
        }
        self
    }
    /// Register a provider and a source under the same name.
    pub fn provider(self, name: impl Into<String>, provider: Contribution) -> Self {
        let name = name.into();
        self.register_provider(name.clone(), provider)
            .source(name.clone(), Source::provider(name))
    }
    /// Register a connection already established by the host's MCP/OAuth client.
    pub fn register_connection(
        mut self,
        endpoint: impl Into<String>,
        credential: impl Into<String>,
        contribution: Contribution,
    ) -> Self {
        let key = (endpoint.into(), credential.into());
        if self.connections.insert(key.clone(), contribution).is_some() {
            self.errors.push(format!("duplicate connection {}", key.0));
        }
        self
    }
    /// Add a source to a new or JSON-loaded configuration.
    pub fn source(mut self, name: impl Into<String>, source: Source) -> Self {
        let name = name.into();
        if self.sources.insert(name.clone(), source).is_some() {
            self.errors.push(format!("duplicate source {name}"));
        }
        self
    }
    /// Register a resource policy with a schema checked against selected operations.
    pub fn register_policy(mut self, name: impl Into<String>, policy: ResourcePolicy) -> Self {
        let name = name.into();
        if self.policies.insert(name.clone(), policy).is_some() {
            self.errors.push(format!("duplicate policy {name}"));
        }
        self
    }
    /// Add an exposure using the same model deserialised from JSON.
    pub fn expose(mut self, operation: Exposure) -> Self {
        self.operations.push(operation);
        self
    }
    /// Resolve registrations, validate restrictions and atomically compile one set.
    pub fn compose(self, bounds: Bounds) -> Result<ComposedSet, CompositionError> {
        self.compose_with_options(bounds, ComposeOptions::default())
    }
    /// Compose with additional host policy and application metadata.
    pub fn compose_with_options(
        mut self,
        bounds: Bounds,
        mut options: ComposeOptions,
    ) -> Result<ComposedSet, CompositionError> {
        if !self.errors.is_empty() {
            return Err(invalid(self.errors.join("; ")));
        }
        if self.oauth.issuer.trim().is_empty() || self.oauth.audience.trim().is_empty() {
            return Err(invalid("OAuth issuer and audience are required"));
        }
        if options
            .oauth
            .as_ref()
            .is_some_and(|oauth| oauth != &self.oauth)
        {
            return Err(invalid("conflicting OAuth resource binding"));
        }
        options.oauth = Some(self.oauth);
        let mut available = BTreeMap::new();
        let mut origins = BTreeSet::new();
        for (source_name, source) in self.sources {
            let (origin, contribution) = match source {
                Source::Provider { provider } => (
                    (0, provider.clone(), String::new()),
                    self.providers.remove(&provider),
                ),
                Source::Remote {
                    endpoint,
                    credential,
                } => (
                    (1, endpoint.clone(), credential.clone()),
                    self.connections.remove(&(endpoint, credential)),
                ),
            };
            if !origins.insert(origin) {
                return Err(invalid(
                    "a registration cannot be claimed by multiple sources",
                ));
            }
            let contribution =
                contribution.ok_or_else(|| invalid(format!("unresolved source {source_name}")))?;
            let mut names = BTreeMap::new();
            for (descriptor, handler) in contribution.operations {
                if descriptor.name.namespace() != Some(contribution.namespace.as_str()) {
                    return Err(invalid("provider operation violates namespace ownership"));
                }
                if names
                    .insert(descriptor.name.to_string(), (descriptor, handler))
                    .is_some()
                {
                    return Err(invalid("duplicate provider operation"));
                }
            }
            available.insert(source_name, (names, contribution.binding_revision));
        }
        let mut contributions = BTreeMap::<String, Contribution>::new();
        let mut namespace_sources = BTreeMap::new();
        let mut selected = BTreeSet::new();
        let mut checks = BTreeSet::new();
        for exposure in self.operations {
            if !selected.insert((exposure.source.clone(), exposure.name.clone())) {
                return Err(invalid("duplicate selected operation"));
            }
            let (names, binding) = available
                .get_mut(&exposure.source)
                .ok_or_else(|| invalid(format!("unknown source {}", exposure.source)))?;
            let (mut descriptor, handler) = names
                .remove(&exposure.name)
                .ok_or_else(|| invalid(format!("unknown operation {}", exposure.name)))?;
            let original = descriptor.name.clone();
            let alias = exposure.expose_as.unwrap_or_else(|| exposure.name.clone());
            descriptor.name = alias.parse().map_err(|_| invalid("invalid alias"))?;
            let namespace = descriptor
                .name
                .namespace()
                .ok_or_else(|| invalid("operation alias requires a namespace"))?
                .to_owned();
            if namespace_sources
                .insert(namespace.clone(), exposure.source.clone())
                .is_some_and(|source| source != exposure.source)
            {
                return Err(invalid("namespace claimed by different sources"));
            }
            descriptor.scopes.extend(exposure.require.scopes);
            if let Some(policy) = exposure.require.policy {
                descriptor.policies.push(policy);
            }
            descriptor.scopes.sort();
            descriptor.scopes.dedup();
            descriptor.policies.sort();
            descriptor.policies.dedup();
            if exposure.cli.confirm {
                descriptor.confirmation = true;
            }
            if let Some(positional) = exposure.cli.positional {
                descriptor.positional = positional;
            }
            let properties = descriptor
                .input
                .get("properties")
                .and_then(serde_json::Value::as_object);
            let mut positions = BTreeSet::new();
            if descriptor.positional.iter().any(|name| {
                !positions.insert(name)
                    || !properties.is_some_and(|fields| fields.contains_key(name))
            }) {
                return Err(invalid("invalid positional property"));
            }
            let contribution = contributions
                .entry(namespace.clone())
                .or_insert_with(|| Contribution::new(namespace));
            contribution.binding_revision = binding.clone();
            if !checks.insert(alias) {
                return Err(invalid("duplicate exposed name"));
            }
            contribution
                .operations
                .push((descriptor, Box::new(Alias { original, handler })));
        }
        if self
            .policies
            .keys()
            .any(|name| options.policies.contains_key(name))
        {
            return Err(invalid("duplicate resource policy registration"));
        }
        options.policies.extend(self.policies);
        compose_with_options(contributions.into_values(), bounds, options)
    }
}

struct Alias {
    original: OperationName,
    handler: Box<dyn Handler>,
}
impl Handler for Alias {
    fn call(&self, mut call: Call) -> HandlerFuture<'_> {
        call.name = self.original.clone();
        self.handler.call(call)
    }
    fn call_checked(&self, mut call: Call) -> crate::CheckedHandlerFuture<'_> {
        call.name = self.original.clone();
        self.handler.call_checked(call)
    }
    fn validate_conversion(
        &self,
        input: &serde_json::Value,
    ) -> Result<(), crate::ConversionFailure> {
        self.handler.validate_conversion(input)
    }
}

fn invalid(message: impl Into<String>) -> CompositionError {
    CompositionError::new(CompositionCode::InvalidConfiguration, message)
}

fn unique_sources<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, Source>, D::Error> {
    struct Sources;
    impl<'de> serde::de::Visitor<'de> for Sources {
        type Value = BTreeMap<String, Source>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("unique source names")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            let mut sources = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, Source>()? {
                if sources.insert(key.clone(), value).is_some() {
                    return Err(serde::de::Error::custom(format!("duplicate source {key}")));
                }
            }
            Ok(sources)
        }
    }
    deserializer.deserialize_map(Sources)
}
