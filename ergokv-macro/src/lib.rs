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
#[proc_macro_derive(Store, attributes(store, key, index))]
pub fn derive_store(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;

    let fields = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(fields) => &fields.named,
            _ => panic!("Only named fields are supported"),
        },
        _ => panic!("Only structs are supported"),
    };

    let load_method = generate_load_method(name, fields);
    let save_method = generate_save_method(name, fields);
    let delete_method = generate_delete_method(name, fields);
    let index_methods = generate_index_methods(name, fields);
    let set_methods = generate_set_methods(name, fields);

    quote! {
        impl #name {
            #load_method
            #save_method
            #delete_method
            #(#index_methods)*
            #(#set_methods)*
        }
    }
    .into()
}

fn generate_load_method(
    name: &Ident,
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
                let key = format!("{}:{}:{}", stringify!(#name).to_lowercase(), key, stringify!(#field_name));
                let value = txn.get(key.clone()).await?
                    .ok_or_else(|| tikv_client::Error::StringError(key.clone()))?;
                ::ergokv::ciborium::de::from_reader(value.as_slice())
                    .map_err(|e| tikv_client::Error::StringError(format!("Failed to decode {}: {}", stringify!(#field_name), e)))?
            };
        }
    });

    let struct_init = fields.iter().map(|f| {
        let field_name = &f.ident;
        quote! { #field_name: #field_name }
    });

    quote! {
        #[doc = concat!("Load a ", stringify!(#name), " from the database.")]
        #[doc = ""]
        #[doc = "This method retrieves the object from TiKV using the provided key."]
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
) -> TokenStream2 {
    let key_field = fields
        .iter()
        .find(|f| {
            f.attrs.iter().any(|a| a.path().is_ident("key"))
        })
        .expect("A field with #[key] attribute is required");
    let key_ident = &key_field.ident;

    let field_saves = fields.iter().map(|f| {
        let field_name = &f.ident;
        quote! {
            let key = format!("{}:{}:{}", stringify!(#name).to_lowercase(), self.#key_ident, stringify!(#field_name));
            let mut value = Vec::new();
            ::ergokv::ciborium::ser::into_writer(&self.#field_name, &mut value)
                .map_err(|e| tikv_client::Error::StringError(format!("Failed to encode {}: {}", stringify!(#field_name), e)))?;
            txn.put(key, value).await?;
        }
    });

    let index_saves = fields.iter()
        .filter(|f| f.attrs.iter().any(|a| a.path().is_ident("index")))
        .map(|f| {
            let field_name = &f.ident;
            quote! {
                let index_key = format!("{}:{}:{}", stringify!(#name).to_lowercase(), stringify!(#field_name), self.#field_name);
                let mut value = Vec::new();
                ::ergokv::ciborium::ser::into_writer(&self.#key_ident, &mut value)
                    .map_err(|e| tikv_client::Error::StringError(format!("Failed to encode {}: {}", stringify!(#field_name), e)))?;
                txn.put(index_key, value).await?;
            }
        });

    quote! {
        #[doc = concat!("Save this ", stringify!(#name), " to the database.")]
        #[doc = ""]
        #[doc = "This method stores the object in TiKV, including any indexed fields."]
        pub async fn save(&self, txn: &mut tikv_client::Transaction) -> Result<(), tikv_client::Error> {
            #(#field_saves)*
            #(#index_saves)*
            Ok(())
        }
    }
}

fn generate_delete_method(
    name: &Ident,
    fields: &Punctuated<Field, Comma>,
) -> TokenStream2 {
    let key_field = fields
        .iter()
        .find(|f| {
            f.attrs.iter().any(|a| a.path().is_ident("key"))
        })
        .expect("A field with #[key] attribute is required");
    let key_ident = &key_field.ident;

    let field_deletes = fields.iter().map(|f| {
        let field_name = &f.ident;
        quote! {
            let key = format!("{}:{}:{}", stringify!(#name).to_lowercase(), self.#key_ident, stringify!(#field_name));
            txn.delete(key).await?;
        }
    });

    let index_deletes = fields.iter()
        .filter(|f| f.attrs.iter().any(|a| a.path().is_ident("index")))
        .map(|f| {
            let field_name = &f.ident;
            quote! {
                let index_key = format!("{}:{}:{}", stringify!(#name).to_lowercase(), stringify!(#field_name), self.#field_name);
                txn.delete(index_key).await?;
            }
        });

    quote! {
        #[doc = concat!("Delete this ", stringify!(#name), " from the database.")]
        #[doc = ""]
        #[doc = "This method removes the object from TiKV, including any indexed fields."]
        pub async fn delete(&self, txn: &mut tikv_client::Transaction) -> Result<(), tikv_client::Error> {
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
    fields.iter()
        .filter(|f| f.attrs.iter().any(|a| a.path().is_ident("index")))
        .map(|f| {
            let field_name = &f.ident;
            let field_type = &f.ty;
            let method_name = format_ident!("by_{}", field_name.clone().expect("Missing field name"));

            quote! {
                #[doc = concat!("Find a ", stringify!(#name), " by its ", stringify!(#field_name), " field.")]
                #[doc = ""]
                #[doc = concat!("This method uses the index on the ", stringify!(#field_name), " field to efficiently retrieve the object.")]
                pub async fn #method_name(value: &#field_type, client: &mut tikv_client::Transaction) -> Result<Option<Self>, tikv_client::Error> {
                    let index_key = format!("{}:{}:{}", stringify!(#name).to_lowercase(), stringify!(#field_name), value);
                    if let Some(key_bytes) = client.get(index_key).await? {
                        let key = ::ergokv::ciborium::de::from_reader(key_bytes.as_slice())
                            .map_err(|e| tikv_client::Error::StringError(format!("Failed to decode key: {}", e)))?;
                        Self::load(&key, client).await.map(Some)
                    } else {
                        Ok(None)
                    }
                }
            }
        })
        .collect()
}

fn generate_set_methods(
    name: &Ident,
    fields: &Punctuated<Field, Comma>,
) -> Vec<TokenStream2> {
    fields.iter().map(|f| {
        let field_name = &f.ident;
        let field_type = &f.ty;
        let method_name = format_ident!("set_{}", field_name.clone().expect("Missing field name"));
        let is_indexed = f.attrs.iter().any(|a| a.path().is_ident("index"));
        let key_field = fields.iter().find(|f| f.attrs.iter().any(|a| a.path().is_ident("key")))
            .expect("A field with #[key] attribute is required");
        let key_ident = &key_field.ident;

        let index_ops = if is_indexed {
            quote! {
                // Remove old index
                let old_index_key = format!("{}:{}:{}", stringify!(#name).to_lowercase(), stringify!(#field_name), self.#field_name);
                txn.delete(old_index_key).await?;

                // Add new index after update
                let new_index_key = format!("{}:{}:{}", stringify!(#name).to_lowercase(), stringify!(#field_name), self.#field_name);
                let mut value = Vec::new();
                ::ergokv::ciborium::ser::into_writer(&self.#key_ident, &mut value)
                    .map_err(|e| tikv_client::Error::StringError(format!("Failed to encode key: {}", e)))?;
                txn.put(new_index_key, value).await?;
            }
        } else {
            quote! {}
        };

        quote! {
            #[doc = concat!("Update the ", stringify!(#field_name), " field of this ", stringify!(#name), ".")]
            #[doc = ""]
            #[doc = concat!("This method updates the ", stringify!(#field_name), " field in the database, maintaining any necessary indexes.")]
            pub async fn #method_name(&mut self, new_value: #field_type, txn: &mut tikv_client::Transaction) -> Result<(), tikv_client::Error> {
                #index_ops

                // Update field
                self.#field_name = new_value;

                // Save updated field
                let key = format!("{}:{}:{}", stringify!(#name).to_lowercase(), self.#key_ident, stringify!(#field_name));
                let mut value = Vec::new();
                ::ergokv::ciborium::ser::into_writer(&self.#field_name, &mut value)
                    .map_err(|e| tikv_client::Error::StringError(format!("Failed to encode {}: {}", stringify!(#field_name), e)))?;
                txn.put(key, value).await?;

                Ok(())
            }
        }
    }).collect()
}
