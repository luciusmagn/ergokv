//! This is never meant to be imported directly :)
//!
//! Use the main `ergokv` crate
use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{
    parse_macro_input, punctuated::Punctuated, token::Comma,
    Data, DeriveInput, Field, Fields, Ident,
};

/// Derives the `Store` trait for a struct, generating methods for CRUD operations in TiKV.
///
/// This macro will generate the following methods:
/// - `load`: Loads an instance from TiKV.
/// - `save`: Saves the instance to TiKV.
/// - `delete`: Deletes the instance from TiKV.
/// - `by_<field>`: For each indexed field, generates a method to find an instance by that field.
/// - `set_<field>`: For each field, generates a method to update that field.
///
/// # Attributes
///
/// - `#[key]`: Marks a field as the primary key. Required on exactly one field.
/// - `#[index]`: Marks a field as indexed, allowing efficient lookups.
///
/// # Example
///
/// ```rust
/// use ergokv::Store;
/// use serde::{Serialize, Deserialize};
/// use uuid::Uuid;
///
/// #[derive(Store, Serialize, Deserialize)]
/// struct User {
///     #[key]
///     id: Uuid,
///     #[index]
///     username: String,
///     email: String,
/// }
/// ```
#[proc_macro_derive(
    Store,
    attributes(
        store,
        key,
        index,
        unique_index,
        migrate_from,
        model_name
    )
)]
pub fn derive_store(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;

    let prev_type = input
        .attrs
        .iter()
        .find(|attr| attr.path().is_ident("migrate_from"))
        .map(|attr| attr.parse_args::<syn::Path>())
        .transpose()
        .unwrap_or(None);

    let fields = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(fields) => &fields.named,
            _ => panic!("Only named fields are supported"),
        },
        _ => panic!("Only structs are supported"),
    };
    let key_field = fields
        .iter()
        .find(|f| {
            f.attrs.iter().any(|a| a.path().is_ident("key"))
        })
        .expect("A field with #[key] attribute is required");

    let load_method = generate_load_method(fields);
    let save_method =
        generate_save_method(name, fields, prev_type.as_ref());
    let delete_method =
        generate_delete_method(name, fields, prev_type.as_ref());
    let index_methods = generate_index_methods(name, fields);
    let set_methods =
        generate_set_methods(name, fields, prev_type.as_ref());
    let all_method = generate_all_method(key_field);
    let migration_trait = prev_type
        .as_ref()
        .map(|prev| generate_migration_trait(name, prev));
    let ensure_migrations = prev_type
        .as_ref()
        .map_or(
            quote! {
                pub async fn ensure_migrations(_client: &::tikv_client::TransactionClient) -> Result<(), ::tikv_client::Error> {
                    Ok(())
                }
            },
            |prev| generate_ensure_migrations(name, prev)
        );
    let backup_restore = generate_backup_restore_methods();

    // TODO: Add unique_index, which is a field_value->ID mapping (this is currently index) and index, which is a field_value->Vec<ID> mapping
    // TODO: Add search function, which queries a field by predicate -- think about if we can make this fast
    quote! {
        #migration_trait

        impl #name {
            const MODEL_NAME: &'static str = stringify!(#name);

            #load_method
            #save_method
            #delete_method
            #ensure_migrations
            #all_method
            #backup_restore
            #(#index_methods)*
            #(#set_methods)*
        }
    }
    .into()
}

fn generate_load_method(
    fields: &Punctuated<Field, Comma>,
) -> TokenStream2 {
    let key_field = fields
        .iter()
        .find(|f| {
            f.attrs.iter().any(|a| a.path().is_ident("key"))
        })
        .expect("A field with #[key] attribute is required");
    let key_type = &key_field.ty;

    let field_loads = fields.iter().map(|f| {
        let field_name = &f.ident;
        let field_type = &f.ty;
        quote! {
            let #field_name: #field_type = {
                let key = format!(
                    "ergokv:{}:{}:{}",
                    Self::MODEL_NAME,
                    ::ergokv::serde_json::to_string(&key)
                        .map_err(|e| tikv_client::Error::StringError(format!("Failed to encode struct key: {e}")))?,
                    stringify!(#field_name)
                );
                let value = txn.get(key.clone()).await?
                    .ok_or_else(|| tikv_client::Error::StringError(key.clone()))?;
                ::ergokv::ciborium::de::from_reader_with_recursion_limit(value.as_slice(), 2048)
                    .map_err(|e| tikv_client::Error::StringError(format!("Failed to decode {}: {}", stringify!(#field_name), e)))?
            };
        }
    });

    let struct_init = fields.iter().map(|f| {
        let field_name = &f.ident;
        quote! { #field_name: #field_name }
    });

    quote! {
        pub async fn load(key: &#key_type, txn: &mut tikv_client::Transaction) -> Result<Self, tikv_client::Error> {
            #(#field_loads)*
            Ok(Self {
                #(#struct_init,)*
            })
        }
    }
}

fn generate_save_method(
    name: &Ident,
    fields: &Punctuated<Field, Comma>,
    prev_type: Option<&syn::Path>,
) -> TokenStream2 {
    let key_field = fields
        .iter()
        .find(|f| {
            f.attrs.iter().any(|a| a.path().is_ident("key"))
        })
        .expect("A field with #[key] attribute is required");
    let key_ident = &key_field.ident;
    let checks = generate_mutation_checks(name, prev_type);

    let field_saves = fields.iter().map(|f| {
        let field_name = &f.ident;
        quote! {
            let key = format!(
                "ergokv:{}:{}:{}",
                Self::MODEL_NAME,
                ::ergokv::serde_json::to_string(&self.#key_ident)
                    .map_err(|e| tikv_client::Error::StringError(format!("Failed to encode struct key: {}", e)))?,
                stringify!(#field_name)
            );
            let mut value = Vec::new();
            ::ergokv::ciborium::ser::into_writer(&self.#field_name, &mut value)
                .map_err(|e| tikv_client::Error::StringError(format!("Failed to encode {}: {}", stringify!(#field_name), e)))?;
            txn.put(key, value).await?;
        }
    });

    let index_saves = fields.iter()
        .filter(|f| f.attrs.iter().any(|a| a.path().is_ident("unique_index") || a.path().is_ident("index")))
        .map(|f| {
            let field_name = &f.ident;
            let is_unique = f.attrs.iter().any(|a| a.path().is_ident("unique_index"));

            if is_unique {
                quote! {
                    let index_key = format!(
                        "ergokv:{}:unique_index:{}:{}",
                        Self::MODEL_NAME,
                        stringify!(#field_name),
                        ::ergokv::serde_json::to_string(&self.#field_name)
                            .map_err(|e| tikv_client::Error::StringError(format!("Failed to encode struct field: {}", e)))?,
                    );
                    let mut value = Vec::new();
                    ::ergokv::ciborium::ser::into_writer(&self.#key_ident, &mut value)
                        .map_err(|e| tikv_client::Error::StringError(format!("Failed to encode {}: {}", stringify!(#field_name), e)))?;
                    txn.put(index_key, value).await?;
                }
            } else {
                quote! {
                    let index_key = format!(
                        "ergokv:{}:index:{}:{}",
                        Self::MODEL_NAME,
                        stringify!(#field_name),
                        ::ergokv::serde_json::to_string(&self.#field_name)
                            .map_err(|e| tikv_client::Error::StringError(format!("Failed to encode struct field: {}", e)))?,
                    );

                    // Read existing keys
                    let mut keys = if let Some(existing_keys_bytes) = txn.get(index_key.clone()).await? {
                        ::ergokv::ciborium::de::from_reader_with_recursion_limit(existing_keys_bytes.as_slice(), 2048)
                            .map_err(|e| tikv_client::Error::StringError(format!("Failed to decode keys: {}", e)))?
                    } else {
                        Vec::new()
                    };

                    // Add current key if not already present
                    if !keys.contains(&self.#key_ident) {
                        keys.push(self.#key_ident);
                    }

                    // Write updated keys
                    let mut value = Vec::new();
                    ::ergokv::ciborium::ser::into_writer(&keys, &mut value)
                        .map_err(|e| tikv_client::Error::StringError(format!("Failed to encode keys: {}", e)))?;
                    txn.put(index_key, value).await?;
                }
            }
        });

    quote! {
        pub async fn save(&self, txn: &mut tikv_client::Transaction) -> Result<(), tikv_client::Error> {
            #checks

            // Add to master trie
            let trie = ::ergokv::PrefixTrie::new("ergokv:__trie");
            trie.insert(
                txn,
                &format!(
                    "{}:{}",
                    Self::MODEL_NAME,
                    ::ergokv::serde_json::to_string(&self.#key_ident)
                        .map_err(|e| tikv_client::Error::StringError(format!("Failed to encode struct key: {}", e)))?
                )
            ).await?;

            #(#field_saves)*
            #(#index_saves)*
            Ok(())
        }
    }
}

fn generate_delete_method(
    name: &Ident,
    fields: &Punctuated<Field, Comma>,
    prev_type: Option<&syn::Path>,
) -> TokenStream2 {
    let key_field = fields
        .iter()
        .find(|f| {
            f.attrs.iter().any(|a| a.path().is_ident("key"))
        })
        .expect("A field with #[key] attribute is required");
    let key_ident = &key_field.ident;
    let key_type = &key_field.ty;
    let checks = generate_mutation_checks(name, prev_type);

    let field_deletes = fields.iter().map(|f| {
        let field_name = &f.ident;
        quote! {
            let key = format!(
                "ergokv:{}:{}:{}",
                Self::MODEL_NAME,
                ::ergokv::serde_json::to_string(&self.#key_ident)
                    .map_err(|e| tikv_client::Error::StringError(format!("Failed to encode struct key: {}", e)))?,
                stringify!(#field_name)
            );
            txn.delete(key).await?;
        }
    });

    let index_deletes = fields.iter()
        .filter(|f| f.attrs.iter().any(|a| a.path().is_ident("unique_index") || a.path().is_ident("index")))
        .map(|f| {
            let field_name = &f.ident;
            let is_unique = f.attrs.iter().any(|a| a.path().is_ident("unique_index"));

            if is_unique {
                quote! {
                    let index_key = format!(
                        "ergokv:{}:unique_index:{}:{}",
                        Self::MODEL_NAME,
                        stringify!(#field_name),
                        ::ergokv::serde_json::to_string(&self.#field_name)
                            .map_err(|e| tikv_client::Error::StringError(format!("Failed to encode struct field: {}", e)))?,
                    );
                    txn.delete(index_key).await?;
                }
            } else {
                quote! {
                    let index_key = format!(
                        "ergokv:{}:index:{}:{}",
                        Self::MODEL_NAME,
                        stringify!(#field_name),
                        ::ergokv::serde_json::to_string(&self.#field_name)
                            .map_err(|e| tikv_client::Error::StringError(format!("Failed to encode struct field: {}", e)))?,
                    );

                    // Read existing keys
                    if let Some(existing_keys_bytes) = txn.get(index_key.clone()).await? {
                        let mut keys: Vec<#key_type> = ::ergokv::ciborium::de::from_reader_with_recursion_limit(existing_keys_bytes.as_slice(), 2048)
                            .map_err(|e| tikv_client::Error::StringError(format!("Failed to decode keys: {}", e)))?;

                        // Remove current key
                        keys.retain(|k| k != &self.#key_ident);

                        // If keys is empty, delete the index entry
                        if keys.is_empty() {
                            txn.delete(index_key).await?;
                        } else {
                            // Otherwise, update the keys
                            let mut value = Vec::new();
                            ::ergokv::ciborium::ser::into_writer(&keys, &mut value)
                                .map_err(|e| tikv_client::Error::StringError(format!("Failed to encode keys: {}", e)))?;
                            txn.put(index_key, value).await?;
                        }
                    }
                }
            }
        });

    quote! {
        pub async fn delete(&self, txn: &mut tikv_client::Transaction) -> Result<(), tikv_client::Error> {
            #checks

            // Remove from master trie
            let trie = ::ergokv::PrefixTrie::new("ergokv:__trie");
            trie.remove(txn, &format!(
                "{}:{}",
                Self::MODEL_NAME,
                ::ergokv::serde_json::to_string(&self.#key_ident)
                    .map_err(|e| tikv_client::Error::StringError(format!("Failed to encode struct key: {}", e)))?,
            )).await?;

            #(#field_deletes)*
            #(#index_deletes)*
            Ok(())
        }
    }
}

fn generate_index_methods(
    name: &Ident,
    fields: &Punctuated<Field, Comma>,
) -> Vec<TokenStream2> {
    let key_field = fields
        .iter()
        .find(|f| {
            f.attrs.iter().any(|a| a.path().is_ident("key"))
        })
        .expect("A field with #[key] attribute is required");
    let key_type = &key_field.ty;

    fields.iter()
        .filter(|f| f.attrs.iter().any(|a| a.path().is_ident("unique_index") || a.path().is_ident("index")))
        .map(|f| {
            let field_name = &f.ident;
            let field_type = &f.ty;
            let method_name = format_ident!("by_{}", field_name.clone().expect("Missing field name"));
            let is_unique = f.attrs.iter().any(|a| a.path().is_ident("unique_index"));

            if is_unique {
                quote! {
                    #[doc = concat!("Find a ", stringify!(#name), " by its ", stringify!(#field_name), " field.")]
                    #[doc = ""]
                    #[doc = concat!("This method uses the unique index on the ", stringify!(#field_name), " field to efficiently retrieve the object.")]
                    pub async fn #method_name<T: Into<#field_type>>(value: T, client: &mut tikv_client::Transaction) -> Result<Option<Self>, tikv_client::Error> {
                        let index_key = format!(
                            "ergokv:{}:unique_index:{}:{}",
                            Self::MODEL_NAME,
                            stringify!(#field_name),
                            ::ergokv::serde_json::to_string(&value.into())
                                .map_err(|e| tikv_client::Error::StringError(format!("Failed to encode struct value: {e}")))?
                        );
                        if let Some(key_bytes) = client.get(index_key).await? {
                            let key = ::ergokv::ciborium::de::from_reader_with_recursion_limit(key_bytes.as_slice(), 2048)
                                .map_err(|e| tikv_client::Error::StringError(format!("Failed to decode key: {}", e)))?;

                            Self::load(&key, client).await.map(Some)
                        } else {
                            Ok(None)
                        }
                    }
                }
            } else {
                quote! {
                    #[doc = concat!("Find all ", stringify!(#name), " by its ", stringify!(#field_name), " field.")]
                    #[doc = ""]
                    #[doc = concat!("This method uses the index on the ", stringify!(#field_name), " field to efficiently retrieve multiple objects.")]
                    pub async fn #method_name<T: Into<#field_type>>(value: T, client: &mut tikv_client::Transaction) -> Result<Vec<Self>, tikv_client::Error> {
                        let index_key = format!(
                            "ergokv:{}:index:{}:{}",
                            Self::MODEL_NAME,
                            stringify!(#field_name),
                            ::ergokv::serde_json::to_string(&value.into())
                                .map_err(|e| tikv_client::Error::StringError(format!("Failed to encode struct value: {e}")))?
                        );
                        if let Some(keys_bytes) = client.get(index_key).await? {
                            let keys: Vec<#key_type> = ::ergokv::ciborium::de::from_reader_with_recursion_limit(keys_bytes.as_slice(), 2048)
                                .map_err(|e| tikv_client::Error::StringError(format!("Failed to decode keys: {}", e)))?;

                            let mut results = Vec::new();
                            for key in keys {
                                results.push(Self::load(&key, client).await?);
                            }
                            Ok(results)
                        } else {
                            Ok(Vec::new())
                        }
                    }
                }
            }
        })
        .collect()
}

fn generate_set_methods(
    name: &Ident,
    fields: &Punctuated<Field, Comma>,
    prev_type: Option<&syn::Path>,
) -> Vec<TokenStream2> {
    fields.iter().map(|f| {
        let field_name = &f.ident;
        let field_type = &f.ty;
        let method_name = format_ident!("set_{}", field_name.clone().expect("Missing field name"));
        let is_indexed = f.attrs.iter().any(|a| a.path().is_ident("index"));
        let key_field = fields.iter().find(|f| f.attrs.iter().any(|a| a.path().is_ident("key")))
            .expect("A field with #[key] attribute is required");
        let key_ident = &key_field.ident;
        let checks = generate_mutation_checks(name, prev_type);

        let index_ops = if is_indexed {
            quote! {
                // Remove old index
                let old_index_key = format!(
                    "ergokv:{}:{}:{}",
                    Self::MODEL_NAME,
                    stringify!(#field_name),
                    ::ergokv::serde_json::to_string(&self.#field_name)
                        .map_err(|e| tikv_client::Error::StringError(format!("Failed to encode struct field: {}", e)))?,
                );
                txn.delete(old_index_key).await?;

                // Add new index after update
                let new_index_key = format!(
                    "ergokv:{}:{}:{}",
                    Self::MODEL_NAME,
                    stringify!(#field_name),
                    ::ergokv::serde_json::to_string(&self.#field_name)
                        .map_err(|e| tikv_client::Error::StringError(format!("Failed to encode struct field: {}", e)))?,
                );
                let mut value = Vec::new();
                ::ergokv::ciborium::ser::into_writer(&self.#key_ident, &mut value)
                    .map_err(|e| tikv_client::Error::StringError(format!("Failed to encode key: {}", e)))?;
                txn.put(new_index_key, value).await?;
            }
        } else {
            quote! {}
        };

        quote! {
            pub async fn #method_name(&mut self, new_value: #field_type, txn: &mut tikv_client::Transaction) -> Result<(), tikv_client::Error> {
                #checks
                #index_ops

                // Update field
                self.#field_name = new_value;

                // Save updated field
                let key = format!(
                    "ergokv:{}:{}:{}",
                    Self::MODEL_NAME,
                    ::ergokv::serde_json::to_string(&self.#key_ident)
                        .map_err(|e| tikv_client::Error::StringError(format!("Failed to encode struct key: {}", e)))?,
                    stringify!(#field_name)
                );
                let mut value = Vec::new();
                ::ergokv::ciborium::ser::into_writer(&self.#field_name, &mut value)
                    .map_err(|e| tikv_client::Error::StringError(format!("Failed to encode {}: {}", stringify!(#field_name), e)))?;
                txn.put(key, value).await?;

                Ok(())
            }
        }
    }).collect()
}

fn generate_all_method(key_field: &Field) -> TokenStream2 {
    let key_type = &key_field.ty;

    quote! {
        pub fn all(txn: &mut tikv_client::Transaction) -> impl futures::Stream<Item = Result<Self, tikv_client::Error>> + '_ {
            use futures::StreamExt;
            let trie = ::ergokv::PrefixTrie::new("ergokv:__trie");

            async_stream::try_stream! {
                let keys = trie.find_by_prefix(txn, Self::MODEL_NAME).await?;
                for key in keys {
                    if let Some(stripped) = key.strip_prefix(&format!("{}:", Self::MODEL_NAME)) {
                        let key: #key_type = ::ergokv::serde_json::from_str(stripped)
                            .map_err(|e| tikv_client::Error::StringError(format!("Failed to decode key: {}", e)))?;
                        yield Self::load(&key, txn).await?;
                    }
                }
            }
        }
    }
}

fn generate_migration_trait(
    name: &Ident,
    prev_type: &syn::Path,
) -> TokenStream2 {
    // Convert path segments to trait name parts
    let trait_name = format_ident!(
        "{}To{}",
        prev_type.segments.last().unwrap().ident,
        name
    );

    // Method name uses lowercase
    let method_name = format_ident!(
        "from_{}",
        prev_type
            .segments
            .last()
            .unwrap()
            .ident
            .to_string()
            .to_lowercase()
    );

    quote! {
        pub trait #trait_name {
            fn #method_name(prev: &#prev_type) -> Result<Self, ::tikv_client::Error>
            where Self: Sized;
        }
    }
}

fn generate_ensure_migrations(
    name: &Ident,
    prev_type: &syn::Path,
) -> TokenStream2 {
    let migration_name = format!(
        "{}->{}",
        prev_type.segments.last().unwrap().ident,
        name
    );
    let method_name = format_ident!(
        "from_{}",
        prev_type
            .segments
            .last()
            .unwrap()
            .ident
            .to_string()
            .to_lowercase()
    );

    quote! {
        pub async fn ensure_migrations(client: &::tikv_client::TransactionClient) -> Result<(), ::tikv_client::Error> {
            let migrations_key = format!("{}:__migrations", Self::MODEL_NAME);
            let mut txn = client.begin_optimistic().await?;

            let migrations: Vec<String> = if let Some(data) = txn.get(migrations_key.as_bytes().to_vec()).await? {
                ::ergokv::ciborium::de::from_reader_with_recursion_limit(&data[..], 2048)
                    .map_err(|e| ::tikv_client::Error::StringError(format!("{e}")))?
            } else {
                Vec::new()
            };

            txn.commit().await?;

            if !migrations.contains(&#migration_name.to_string()) {
                #prev_type::ensure_migrations(&client).await?;

                let mut txn = client.begin_optimistic().await?;
                let mut stream = Box::pin(#prev_type::all(&mut txn));

                // TODO: We are saving over the old data, but unused fields may linger
                {
                    use ::ergokv::futures::StreamExt;
                    let mut stream = stream;
                    while let Some(Ok(prev_item)) = stream.next().await {
                        let mut new_txn = client.begin_optimistic().await?;

                        match Self::#method_name(&prev_item) {
                            Ok(new) => {
                                new.save(&mut new_txn).await?;
                                new_txn.commit().await?;
                            }
                            e @ Err(_) => {
                                new_txn.rollback().await?;
                                e?;
                            }
                        };
                    }
                }

                let mut new_migrations = migrations;
                new_migrations.push(#migration_name.to_string());

                let mut buf = vec![];
                ::ergokv::ciborium::ser::into_writer(&new_migrations, &mut buf)
                    .map_err(|e| ::tikv_client::Error::StringError(format!("{e}")))?;

                txn.put(migrations_key.as_bytes().to_vec(), buf).await?;

                txn.commit().await?;
            }

            Ok(())
        }
    }
}

fn generate_mutation_checks(
    name: &Ident,
    prev_type: Option<&syn::Path>,
) -> TokenStream2 {
    #[cfg(feature = "strict-migrations")]
    {
        let prev_check = prev_type.map(|prev| {
            quote! {
                if !migrations.contains(&format!("{}->{}",
                    stringify!(#prev),
                    stringify!(#name)
                )) {
                    return Err(::tikv_client::Error::StringError(
                        format!("Previous migration {}=>{} not applied", stringify!(#prev), stringify!(#name))
                    ));
                }
            }
        });

        quote! {
            let migrations_key = format!("{}:__migrations", Self::MODEL_NAME);
            let migrations: Vec<String> = if let Some(data) = txn.get(&migrations_key).await? {
                ::ergokv::ciborium::de::from_reader_with_recursion_limit(&data[..], 2048)?
            } else {
                Vec::new()
            };

            if migrations.iter().any(|m| m.starts_with(&format!("{}->", stringify!(#name)))) {
                return Err(::tikv_client::Error::StringError(
                    format!("Cannot modify {} - newer version exists", stringify!(#name))
                ));
            }

            #prev_check
        }
    }

    #[cfg(not(feature = "strict-migrations"))]
    {
        // so we can prevent a warning about the params being unused with strict migrations
        let _unused_prev = prev_type;
        let _unused_name = name;
        quote! {}
    }
}

// TODO: Consider using RON instead, or providing it as an option
fn generate_backup_restore_methods() -> TokenStream2 {
    quote! {
         /// Creates a backup of all instances of this type in JSON format.
         ///
         /// The backup is stored in a file named `{MODEL_NAME}_{timestamp}.json` under the specified path,
         /// where timestamp is the Unix epoch time in seconds. Each line in the file contains one JSON-serialized
         /// instance.
         ///
         /// # Arguments
         ///
         /// * `txn` - TiKV transaction to use for reading the data
         /// * `path` - Directory where the backup file will be created
         ///
         /// # Returns
         ///
         /// Returns the full path to the created backup file.
         ///
         /// # Errors
         ///
         /// This function will return an error if:
         /// - The backup directory is not writable
         /// - Any instance fails to serialize to JSON
         /// - The TiKV transaction fails
         ///
         /// # Example
         ///
         /// ```no_run
         /// # use ergokv::Store;
         /// # use tikv_client::TransactionClient;
         /// # #[derive(Store)]
         /// # struct User { }
         /// # async fn example() -> Result<(), tikv_client::Error> {
         /// # let client = TransactionClient::new(vec!["127.0.0.1:2379"]).await?;
         /// let mut txn = client.begin_optimistic().await?;
         /// let backup_path = User::backup(&mut txn, "backups/").await?;
         /// println!("Backup created at: {}", backup_path.display());
         /// txn.commit().await?;
         /// # Ok(())
         /// # }
         /// ```
         pub async fn backup(txn: &mut tikv_client::Transaction, path: impl AsRef<std::path::Path>) -> Result<std::path::PathBuf, tikv_client::Error> {
            use std::io::Write;
            use futures::StreamExt;

            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| tikv_client::Error::StringError(e.to_string()))?
                .as_secs();

            let filename = format!("{}_{}.json", Self::MODEL_NAME, timestamp);
            let backup_path = path.as_ref().join(filename);

            let mut file = std::fs::File::create(&backup_path)
                .map_err(|e| tikv_client::Error::StringError(format!("Failed to create backup file: {}", e)))?;

            let mut stream = Box::pin(Self::all(txn));
            while let Some(item) = stream.next().await {
                let item = item?;
                let json = serde_json::to_string(&item)
                    .map_err(|e| tikv_client::Error::StringError(format!("Failed to serialize: {}", e)))?;
                writeln!(file, "{}", json)
                    .map_err(|e| tikv_client::Error::StringError(format!("Failed to write: {}", e)))?;
            }

            Ok(backup_path)
        }

        /// Restores instances from a backup file created by [`backup`](Self::backup).
        ///
        /// Reads the backup file line by line, deserializing each line as an instance
        /// and saving it to TiKV. The operation is performed within the provided transaction,
        /// allowing you to control atomicity.
        ///
        /// # Arguments
        ///
        /// * `txn` - TiKV transaction to use for writing the data
        /// * `path` - Path to the backup file
        ///
        /// # Errors
        ///
        /// This function will return an error if:
        /// - The backup file cannot be read
        /// - Any line fails to deserialize from JSON
        /// - The TiKV transaction fails
        /// - Any instance fails to save
        ///
        /// # Warning
        ///
        /// This operation will overwrite any existing instances with the same keys.
        /// Make sure to handle any potential conflicts before restoring.
        ///
        /// # Example
        ///
        /// ```no_run
        /// # use ergokv::Store;
        /// # use tikv_client::TransactionClient;
        /// # #[derive(Store)]
        /// # struct User { }
        /// # async fn example() -> Result<(), tikv_client::Error> {
        /// # let client = TransactionClient::new(vec!["127.0.0.1:2379"]).await?;
        /// let mut txn = client.begin_optimistic().await?;
        /// User::restore(&mut txn, "backups/User_1234567890.json").await?;
        /// txn.commit().await?;
        /// # Ok(())
        /// # }
        /// ```
        pub async fn restore(txn: &mut tikv_client::Transaction, path: impl AsRef<std::path::Path>) -> Result<(), tikv_client::Error> {
            use std::io::BufRead;

            let file = std::fs::File::open(path)
                .map_err(|e| tikv_client::Error::StringError(format!("Failed to open backup file: {}", e)))?;

            let reader = std::io::BufReader::new(file);
            for line in reader.lines() {
                let line = line
                    .map_err(|e| tikv_client::Error::StringError(format!("Failed to read line: {}", e)))?;

                let item: Self = serde_json::from_str(&line)
                    .map_err(|e| tikv_client::Error::StringError(format!("Failed to deserialize: {}", e)))?;

                item.save(txn).await?;
            }

            Ok(())
        }
    }
}
