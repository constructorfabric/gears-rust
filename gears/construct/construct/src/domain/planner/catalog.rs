use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::sync::{Arc, LazyLock};

use construct_sdk::linked_schema::{LinkKind, NodeKind, Schema};
use jsonschema::{Draft, Validator};
use lru::LruCache;
use parking_lot::Mutex;
use serde_json::Value;
use toolkit_macros::domain_model;

use crate::domain::error::DomainError;
use crate::domain::profile::Category;
use crate::domain::profile_schema::ProfileSchemas;
use crate::domain::subject_settings::SubjectKey;

/// A JSON schema with its compiled validator.
#[domain_model]
#[derive(Debug)]
struct Checked {
    schema: Value,
    validator: Validator,
}

impl Checked {
    fn compile(name: &str, schema: Value) -> Result<Self, DomainError> {
        let validator = jsonschema::options()
            .with_draft(Draft::Draft7)
            .build(&schema)
            .map_err(|error| {
                DomainError::internal(format!("the schema of {name} does not compile: {error}"))
            })?;
        Ok(Self { schema, validator })
    }

    fn check(&self, value: &Value) -> Result<(), String> {
        let Some(error) = self.validator.iter_errors(value).next() else {
            return Ok(());
        };
        let rule = error
            .schema_path()
            .as_str()
            .rsplit('/')
            .next()
            .unwrap_or_default();
        let place = match error.instance_path().as_str() {
            "" => "the value".to_owned(),
            path => format!("`{path}`"),
        };
        Err(format!("{place} breaks `{rule}`; it takes {}", self.schema))
    }
}

/// One node kind: its category and the schema of the object of fields a node of it holds.
#[domain_model]
#[derive(Debug)]
pub struct KindSpec {
    pub category: Category,
    kind: NodeKind,
    checked: Checked,
}

impl KindSpec {
    /// The kind in the schema.
    #[must_use]
    pub fn kind(&self) -> &NodeKind {
        &self.kind
    }

    /// The JSON schema of the kind's fields.
    #[must_use]
    pub fn schema(&self) -> &Value {
        &self.checked.schema
    }

    /// Checks the fields of a node against the kind's schema.
    ///
    /// # Errors
    ///
    /// The first rule the value breaks and where, followed by the schema the kind takes. The reason does not repeat
    /// the value.
    pub fn check(&self, value: &Value) -> Result<(), String> {
        self.checked.check(value)
    }
}

/// One link kind and the schema of its fields.
#[domain_model]
#[derive(Debug)]
pub struct LinkSpec {
    kind: LinkKind,
    checked: Checked,
}

impl LinkSpec {
    /// The link kind in the schema.
    #[must_use]
    pub fn kind(&self) -> &LinkKind {
        &self.kind
    }

    /// The JSON schema of the link's fields.
    #[must_use]
    pub fn schema(&self) -> &Value {
        &self.checked.schema
    }

    /// Checks the fields of a link against the link kind's schema.
    ///
    /// # Errors
    ///
    /// As [`KindSpec::check`].
    pub fn check(&self, value: &Value) -> Result<(), String> {
        self.checked.check(value)
    }
}

/// Every node kind and link kind of the profile schema, by name, with its schema compiled.
#[domain_model]
#[derive(Debug)]
pub struct Catalog {
    kinds: BTreeMap<&'static str, KindSpec>,
    links: BTreeMap<&'static str, LinkSpec>,
}

impl Catalog {
    /// Reads every kind and link kind from `schema`.
    ///
    /// # Errors
    ///
    /// [`DomainError::Internal`] when a schema does not compile.
    pub fn load(schema: &Schema) -> Result<Self, DomainError> {
        let mut kinds = BTreeMap::new();
        for kind in &schema.nodes {
            let category = Category::from_name(schema, kind.name).ok_or_else(|| {
                DomainError::internal(format!("{} is not a kind of the schema", kind.name))
            })?;
            let checked = Checked::compile(kind.name, Schema::fields_schema(kind))?;
            kinds.insert(
                kind.name,
                KindSpec {
                    category,
                    kind: kind.clone(),
                    checked,
                },
            );
        }
        let mut links = BTreeMap::new();
        for kind in &schema.links {
            let checked = Checked::compile(kind.name, Schema::link_fields_schema(kind))?;
            links.insert(
                kind.name,
                LinkSpec {
                    kind: kind.clone(),
                    checked,
                },
            );
        }
        Ok(Self { kinds, links })
    }

    /// The node kind named `name`, or `None` when the schema has no such kind.
    #[must_use]
    pub fn kind(&self, name: &str) -> Option<&KindSpec> {
        self.kinds.get(name)
    }

    /// The node kind of `category`.
    ///
    /// # Panics
    ///
    /// Never for a category of this catalog's schema, which is the only kind of category a draft over it holds.
    #[must_use]
    #[allow(
        clippy::expect_used,
        reason = "a category is only ever made from a kind of the schema the catalog was loaded from"
    )]
    pub fn node(&self, category: Category) -> &NodeKind {
        self.kinds
            .get(category.name())
            .expect("a category is a kind of the catalog's schema")
            .kind()
    }

    /// The link kind named `name`, or `None` when the schema has no such link kind.
    #[must_use]
    pub fn link(&self, name: &str) -> Option<&LinkSpec> {
        self.links.get(name)
    }

    /// The names of all node kinds, in alphabetical order.
    pub fn kind_names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.kinds.keys().copied()
    }

    /// The names of all link kinds, in alphabetical order.
    pub fn link_names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.links.keys().copied()
    }
}

/// How many distinct schemas have their compiled catalog kept, for the whole process: people with one schema share one
/// compile, and the one used least recently goes first.
const SHARED_KEPT: NonZeroUsize = NonZeroUsize::MIN.saturating_add(255);

/// How many people have their schema and catalog at hand; the one used least recently goes first.
const KEPT: NonZeroUsize = NonZeroUsize::MIN.saturating_add(1023);

/// The catalogs by the schema they were compiled from, for the whole process.
static SHARED: LazyLock<Mutex<LruCache<String, Arc<Catalog>>>> =
    LazyLock::new(|| Mutex::new(LruCache::new(SHARED_KEPT)));

/// A person's schema and the catalog compiled from it.
type Kept = (Arc<Schema>, Arc<Catalog>);

/// The catalog of each person's schema. A catalog compiles the JSON schema of every kind, so one is kept for as long
/// as the person's schema is the one it was compiled from, and compiled again once the schema changed.
#[domain_model]
pub struct Catalogs {
    schemas: Arc<dyn ProfileSchemas>,
    kept: Mutex<LruCache<SubjectKey, Kept>>,
}

impl Catalogs {
    #[must_use]
    pub fn new(schemas: Arc<dyn ProfileSchemas>) -> Self {
        Self {
            schemas,
            kept: Mutex::new(LruCache::new(KEPT)),
        }
    }

    /// The catalog of the person's schema as it is now.
    ///
    /// # Errors
    ///
    /// When the schema cannot be read, or a schema of it does not compile.
    pub async fn of(&self, key: SubjectKey) -> Result<Arc<Catalog>, DomainError> {
        let schema = self.schemas.schema(key).await?;
        if let Some((held, catalog)) = self.kept.lock().get(&key)
            && **held == *schema
        {
            return Ok(Arc::clone(catalog));
        }
        let content = crate::domain::profile_schema::doc_json(
            &construct_sdk::schema_doc::SchemaDoc::of(&schema),
        )?;
        let known = SHARED.lock().get(&content).map(Arc::clone);
        let catalog = if let Some(catalog) = known {
            catalog
        } else {
            let compiled = Arc::new(Catalog::load(&schema)?);
            SHARED.lock().put(content, Arc::clone(&compiled));
            compiled
        };
        self.kept.lock().put(key, (schema, Arc::clone(&catalog)));
        Ok(catalog)
    }
}
