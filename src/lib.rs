use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{
    parse_macro_input, punctuated::Punctuated, token::Comma,
    Data, DeriveInput, Field, Fields, Ident,
};

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
                serde_cbor::from_slice(&value)
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
            let value = serde_cbor::to_vec(&self.#field_name)
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
                let value = serde_cbor::to_vec(&self.#key_ident)
                    .map_err(|e| tikv_client::Error::StringError(format!("Failed to encode {}: {}", stringify!(#field_name), e)))?;
                txn.put(index_key, value).await?;
            }
        });

    quote! {
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
                pub async fn #method_name(value: &#field_type, client: &mut tikv_client::Transaction) -> Result<Option<Self>, tikv_client::Error> {
                    let index_key = dbg!(format!("{}:{}:{}", stringify!(#name).to_lowercase(), stringify!(#field_name), value));
                    if let Some(key) = client
                        .get(index_key)
                        .await?
                        .and_then(|key| serde_cbor::from_slice(&key).ok())
                    {
                        Self::load(&key, client).await.map(Some)
                    } else {
                        Ok(None)
                    }
                }
            }
        })
        .collect()
}

// TODO: forbid changing the ID
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

        quote! {
            pub async fn #method_name(&mut self, new_value: #field_type, txn: &mut tikv_client::Transaction) -> Result<(), tikv_client::Error> {
                if #is_indexed {
                    // Remove old index
                    let old_index_key = format!("{}:{}:{}", stringify!(#name).to_lowercase(), stringify!(#field_name), self.#field_name);
                    txn.delete(old_index_key).await?;
                }

                // Update field
                self.#field_name = new_value;

                // Save updated field
                let key = format!("{}:{}:{}", stringify!(#name).to_lowercase(), self.#key_ident, stringify!(#field_name));
                let value = serde_cbor::to_vec(&self.#field_name)
                    .map_err(|e| tikv_client::Error::StringError(format!("Failed to encode {}: {}", stringify!(#field_name), e)))?;
                txn.put(key, value).await?;

                if #is_indexed {
                    // Add new index
                    let new_index_key = format!("{}:{}:{}", stringify!(#name).to_lowercase(), stringify!(#field_name), self.#field_name);
                    txn.put(new_index_key, self.#key_ident.to_string()).await?;
                }

                Ok(())
            }
        }
    }).collect()
}
