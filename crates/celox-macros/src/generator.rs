use quote::{format_ident, quote};
use veryl_analyzer::ir::{Component, Ir, VarPath};

pub fn generate_project(ir: &Ir) -> proc_macro2::TokenStream {
    let mut all_expanded = quote! {};

    for component in &ir.components {
        let module = match component {
            Component::Module(m) => m,
            _ => continue,
        };
        let module_name_str = module.name.to_string();

        let module_name = format_ident!("{}", module_name_str);

        // Group ports by interface
        let mut interface_map: std::collections::HashMap<
            String,
            Vec<(&VarPath, &veryl_analyzer::ir::Type)>,
        > = std::collections::HashMap::new();
        let mut top_level_ports = Vec::new();

        for (var_path, _var_id) in &module.ports {
            if let Some((r#type, _clock)) = module.port_types.get(var_path) {
                if var_path.0.len() > 1 {
                    let interface_name = var_path.0[0].to_string();
                    interface_map
                        .entry(interface_name)
                        .or_default()
                        .push((var_path, r#type));
                } else {
                    top_level_ports.push((var_path, r#type));
                }
            }
        }

        let struct_name = &module_name;
        let bound_name = format_ident!("{}Bound", struct_name);
        let io_name = format_ident!("{}IO", struct_name);

        let mut fields = quote! {};
        let mut init_ports = quote! {};
        let mut methods = quote! {};
        let mut bound_methods = quote! {};
        let mut io_methods = quote! {};
        let mut extra_structs = quote! {};

        let mut clk_ident = None;

        // Process top-level ports
        for (var_path, r#type) in top_level_ports {
            let name = veryl_parser::resource_table::get_str_value(*var_path.0.iter().last().unwrap()).unwrap();
            let port_ident = format_ident!("{}", name);
            let is_clk = r#type.is_clock();
            if is_clk {
                clk_ident = Some(port_ident.clone());
            }

            if is_clk {
                fields.extend(quote! { pub #port_ident: celox::EventRef, });
                init_ports.extend(quote! { #port_ident: sim.event(#name), });
                continue; // Do not generate read/write accessors for clk
            } else {
                fields.extend(quote! { pub #port_ident: celox::SignalRef, });
                init_ports.extend(quote! { #port_ident: sim.signal(#name), });
            }

            let element_width = r#type.total_width().unwrap_or(1);
            let array_dims = r#type.array.dims();
            let total_array = r#type.total_array().unwrap_or(1);
            let width = element_width * total_array;
            let is_4state = r#type.is_4state();

            let rust_type = if element_width <= 8 {
                quote! { u8 }
            } else if element_width <= 16 {
                quote! { u16 }
            } else if element_width <= 32 {
                quote! { u32 }
            } else if element_width <= 64 {
                quote! { u64 }
            } else if element_width <= 128 {
                quote! { u128 }
            } else {
                quote! { celox::BigUint }
            };

            let setter_name = format_ident!("set_{}", name);
            let getter_name = format_ident!("get_{}", name);

            if array_dims > 0 {
                methods.extend(quote! {
                    pub fn #setter_name(&self, sim: &mut celox::Simulator, index: usize, val: #rust_type) {
                        let mut val_full = sim.get(self.#port_ident);
                        let element_width = #element_width;
                        let bit_offset = index * element_width;
                        
                        let mask = ((celox::BigUint::from(1u32) << element_width) - 1u32) << bit_offset;
                        
                        val_full = (val_full.clone() ^ (val_full & &mask)) | (celox::BigUint::from(val) << bit_offset);
                        
                        sim.modify(|io| io.set_wide(self.#port_ident, val_full)).unwrap();
                    }
                    pub fn #getter_name(&self, sim: &mut celox::Simulator, index: usize) -> #rust_type {
                        let val_full = sim.get(self.#port_ident);
                        let element_width = #element_width;
                        let bit_offset = index * element_width;
                        let mask = (celox::BigUint::from(1u32) << element_width) - 1u32;
                        let element_val = (val_full >> bit_offset) & mask;
                        element_val.try_into().unwrap_or_else(|_| panic!("Value overflow for {}[{}]", #name, index))
                    }
                });
                bound_methods.extend(quote! {
                    pub fn #setter_name(&mut self, index: usize, val: #rust_type) { self.ids.#setter_name(self.sim, index, val); }
                    pub fn #getter_name(&mut self, index: usize) -> #rust_type { self.ids.#getter_name(self.sim, index) }
                });
                io_methods.extend(quote! {
                    pub fn #setter_name(&mut self, _index: usize, _val: #rust_type) {
                        // TODO: IO setter for arrays
                    }
                });
            } else {
                let setter_body = if width <= 128 {
                    quote! { sim.modify(|io| io.set(self.#port_ident, val)).unwrap(); }
                } else {
                    quote! { sim.modify(|io| io.set_wide(self.#port_ident, val)).unwrap(); }
                };

                methods.extend(quote! {
                    pub fn #setter_name(&self, sim: &mut celox::Simulator, val: #rust_type) {
                        #setter_body
                    }
                    pub fn #getter_name(&self, sim: &mut celox::Simulator) -> #rust_type {
                        let val = sim.get(self.#port_ident);
                        val.try_into().unwrap_or_else(|_| panic!("Value overflow for {}", #name))
                    }
                });

                bound_methods.extend(quote! {
                    pub fn #setter_name(&mut self, val: #rust_type) { self.ids.#setter_name(self.sim, val); }
                    pub fn #getter_name(&mut self) -> #rust_type { self.ids.#getter_name(self.sim) }
                });

                let io_setter_body = if width <= 128 {
                    quote! { self.io.set(self.ids.#port_ident, val); }
                } else {
                    quote! { self.io.set_wide(self.ids.#port_ident, val); }
                };
                io_methods.extend(quote! {
                    pub fn #setter_name(&mut self, val: #rust_type) { #io_setter_body }
                });
            }

            if is_4state {
                let setter_4state = format_ident!("set_{}_4state", name);
                let getter_4state = format_ident!("get_{}_4state", name);
                let big_uint = quote! { celox::BigUint };

                methods.extend(quote! {
                    pub fn #setter_4state(&self, sim: &mut celox::Simulator, val: #big_uint, mask: #big_uint) {
                        sim.modify(|io| io.set_four_state(self.#port_ident, val, mask)).unwrap();
                    }
                    pub fn #getter_4state(&self, sim: &mut celox::Simulator) -> (#big_uint, #big_uint) {
                        sim.get_four_state(self.#port_ident)
                    }
                });

                bound_methods.extend(quote! {
                    pub fn #setter_4state(&mut self, val: #big_uint, mask: #big_uint) {
                        self.ids.#setter_4state(self.sim, val, mask);
                    }
                    pub fn #getter_4state(&mut self) -> (#big_uint, #big_uint) {
                        self.ids.#getter_4state(self.sim)
                    }
                });

                io_methods.extend(quote! {
                    pub fn #setter_4state(&mut self, val: #big_uint, mask: #big_uint) {
                        self.io.set_four_state(self.ids.#port_ident, val, mask);
                    }
                });
            }
        }

        // Process interfaces
        for (if_name, if_ports) in interface_map {
            let if_struct_name = format_ident!("{}", if_name);
            let if_bound_name = format_ident!("{}Bound", if_struct_name);
            let if_io_name = format_ident!("{}IO", if_struct_name);

            let mut if_fields = quote! {};
            let mut if_init = quote! {};
            let mut if_methods = quote! {};
            let mut if_bound_methods = quote! {};
            let mut if_io_methods = quote! {};

            for (var_path, r#type) in if_ports {
                let member_name = var_path.0.last().unwrap().to_string();
                let member_ident = format_ident!("{}", member_name);

                if false {
                    // clk in interface? Veryl allows it but let's assume not for now
                }

                if_fields.extend(quote! { pub #member_ident: celox::SignalRef, });
                if_init.extend(quote! { #member_ident: sim.signal(#member_name), });

                let element_width = r#type.total_width().unwrap_or(1);
                let array_dims = r#type.array.dims();
                let total_array = r#type.total_array().unwrap_or(1);
                let width = element_width * total_array;
                let is_4state = r#type.is_4state();

                let rust_type = if element_width <= 8 {
                    quote! { u8 }
                } else if element_width <= 16 {
                    quote! { u16 }
                } else if element_width <= 32 {
                    quote! { u32 }
                } else if element_width <= 64 {
                    quote! { u64 }
                } else if element_width <= 128 {
                    quote! { u128 }
                } else {
                    quote! { celox::BigUint }
                };

                let setter_name = format_ident!("set_{}", member_name);
                let getter_name = format_ident!("get_{}", member_name);

                if array_dims > 0 {
                    if_methods.extend(quote! {
                        pub fn #setter_name(&self, sim: &mut celox::Simulator, index: usize, val: #rust_type) {
                            let mut val_full = sim.get(self.#member_ident);
                            let element_width = #element_width;
                            let bit_offset = index * element_width;
                            let mask = ((celox::BigUint::from(1u32) << element_width) - 1u32) << bit_offset;
                            val_full = (val_full & !mask) | (celox::BigUint::from(val) << bit_offset);
                            sim.modify(|io| io.set_wide(self.#member_ident, val_full)).unwrap();
                        }
                        pub fn #getter_name(&self, sim: &mut celox::Simulator, index: usize) -> #rust_type {
                            let val_full = sim.get(self.#member_ident);
                            let element_width = #element_width;
                            let bit_offset = index * element_width;
                            let mask = (celox::BigUint::from(1u32) << element_width) - 1u32;
                            let element_val = (val_full >> bit_offset) & mask;
                            element_val.try_into().unwrap()
                        }
                    });
                    if_bound_methods.extend(quote! {
                        pub fn #setter_name(&mut self, index: usize, val: #rust_type) { self.ids.#setter_name(self.sim, index, val); }
                        pub fn #getter_name(&mut self, index: usize) -> #rust_type { self.ids.#getter_name(self.sim, index) }
                    });
                    if_io_methods.extend(quote! {
                        pub fn #setter_name(&mut self, _index: usize, _val: #rust_type) { }
                    });
                } else {
                    let setter_body = if width <= 128 {
                        quote! { sim.modify(|io| io.set(self.#member_ident, val)).unwrap(); }
                    } else {
                        quote! { sim.modify(|io| io.set_wide(self.#member_ident, val)).unwrap(); }
                    };

                    if_methods.extend(quote! {
                        pub fn #setter_name(&self, sim: &mut celox::Simulator, val: #rust_type) {
                            #setter_body
                        }
                        pub fn #getter_name(&self, sim: &mut celox::Simulator) -> #rust_type {
                            let val = sim.get(self.#member_ident);
                            val.try_into().unwrap_or_else(|_| panic!("Value overflow for {}", #member_name))
                        }
                    });

                    if_bound_methods.extend(quote! {
                        pub fn #setter_name(&mut self, val: #rust_type) { self.ids.#setter_name(self.sim, val); }
                        pub fn #getter_name(&mut self) -> #rust_type { self.ids.#getter_name(self.sim) }
                    });

                    let io_setter_body = if width <= 128 {
                        quote! { self.io.set(self.ids.#member_ident, val); }
                    } else {
                        quote! { self.io.set_wide(self.ids.#member_ident, val); }
                    };
                    if_io_methods.extend(quote! {
                        pub fn #setter_name(&mut self, val: #rust_type) { #io_setter_body }
                    });
                }

                if is_4state {
                    let setter_4state = format_ident!("set_{}_4state", member_name);
                    let getter_4state = format_ident!("get_{}_4state", member_name);
                    let big_uint = quote! { celox::BigUint };

                    if_methods.extend(quote! {
                        pub fn #setter_4state(&self, sim: &mut celox::Simulator, val: #big_uint, mask: #big_uint) {
                            sim.modify(|io| io.set_four_state(self.#member_ident, val, mask)).unwrap();
                        }
                        pub fn #getter_4state(&self, sim: &mut celox::Simulator) -> (#big_uint, #big_uint) {
                            sim.get_four_state(self.#member_ident)
                        }
                    });

                    if_bound_methods.extend(quote! {
                        pub fn #setter_4state(&mut self, val: #big_uint, mask: #big_uint) {
                            self.ids.#setter_4state(self.sim, val, mask);
                        }
                        pub fn #getter_4state(&mut self) -> (#big_uint, #big_uint) {
                            self.ids.#getter_4state(self.sim)
                        }
                    });

                    if_io_methods.extend(quote! {
                        pub fn #setter_4state(&mut self, val: #big_uint, mask: #big_uint) {
                            self.io.set_four_state(self.ids.#member_ident, val, mask);
                        }
                    });
                }
            }

            extra_structs.extend(quote! {
                pub struct #if_struct_name { #if_fields }
                impl #if_struct_name { #if_methods }
                pub struct #if_bound_name<'a> { sim: &'a mut celox::Simulator, ids: &'a #if_struct_name }
                impl<'a> #if_bound_name<'a> { #if_bound_methods }
                pub struct #if_io_name<'a, 'b> { io: &'a mut celox::IOContext<'b>, ids: &'a #if_struct_name }
                impl<'a, 'b> #if_io_name<'a, 'b> { #if_io_methods }
            });

            let if_ident = format_ident!("{}", if_name);
            fields.extend(quote! { pub #if_ident: #if_struct_name, });
            init_ports.extend(quote! { #if_ident: #if_struct_name { #if_init }, });
            methods.extend(
                quote! { pub fn #if_ident(&self) -> #if_struct_name { self.#if_ident.clone() } },
            );
            bound_methods.extend(quote! { pub fn #if_ident<'b>(&'b mut self) -> #if_bound_name<'b> { #if_bound_name { sim: self.sim, ids: &self.ids.#if_ident } } });
            io_methods.extend(quote! { pub fn #if_ident<'c>(&'c mut self) -> #if_io_name<'c, '_> { #if_io_name { io: self.io, ids: &self.ids.#if_ident } } });
        }

        let tick_body_main = if let Some(clk) = &clk_ident {
            quote! { sim.tick(self.#clk).unwrap(); }
        } else {
            quote! {}
        };
        let tick_body_bound = if let Some(clk) = &clk_ident {
            quote! { self.sim.tick(self.ids.#clk).unwrap(); }
        } else {
            quote! {}
        };

        let expanded = quote! {
            pub struct #struct_name {
                #fields
            }

            impl #struct_name {
                pub fn new(sim: &celox::Simulator) -> Self {
                    Self {
                        #init_ports
                    }
                }
                #methods
                pub fn bind<'a>(&'a self, sim: &'a mut celox::Simulator) -> #bound_name<'a> {
                    #bound_name { sim, ids: self }
                }
                pub fn tick(&self, sim: &mut celox::Simulator) {
                    #tick_body_main
                }
            }

            #extra_structs

            pub struct #bound_name<'a> {
                sim: &'a mut celox::Simulator,
                ids: &'a #struct_name,
            }

            impl<'a> #bound_name<'a> {
                #bound_methods
                pub fn tick(&mut self) {
                    #tick_body_bound
                }
                pub fn modify<F>(&mut self, f: F)
                where F: FnOnce(&mut #io_name<'_, '_>)
                {
                    self.sim.modify(|io| {
                        let mut a_io = #io_name { io, ids: self.ids };
                        f(&mut a_io);
                    }).unwrap();
                }
            }

            pub struct #io_name<'a, 'b> {
                io: &'a mut celox::IOContext<'b>,
                ids: &'a #struct_name,
            }

            impl<'a, 'b> #io_name<'a, 'b> {
                #io_methods
            }
        };

        all_expanded.extend(expanded);
    }

    all_expanded
}

#[cfg(test)]
mod test_generator;
