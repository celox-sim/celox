use super::*;

impl Parser<'_> {
    pub(super) fn package_binding_start(&self) -> bool {
        matches!(self.peek(), "import" | "export") && !self.dpi_start()
    }

    pub(super) fn package_binding(&mut self) -> Result<PackageBindingId> {
        let start = self.at;
        let kind = if self.eat("import") {
            PackageBindingKind::Import
        } else {
            self.expect("export")?;
            PackageBindingKind::Export
        };
        let mut items = Vec::new();
        loop {
            let item_start = self.at;
            let item = if self.eat("*") {
                if kind != PackageBindingKind::Export || !items.is_empty() {
                    return Err(self.error("only `export *::*;` permits a wildcard package"));
                }
                self.expect("::")?;
                self.expect("*")?;
                PackageBindingItemKind::All
            } else {
                let package = self.name()?;
                self.expect("::")?;
                if self.eat("*") {
                    PackageBindingItemKind::Wildcard(package)
                } else {
                    PackageBindingItemKind::Named {
                        package,
                        name: self.name()?,
                    }
                }
            };
            let all = matches!(item, PackageBindingItemKind::All);
            items.push(PackageBindingItem {
                kind: item,
                tokens: self.range(item_start),
            });
            if items.len() >= 1_000_000 {
                return Err(self.unsupported("native package binding exceeds one million items"));
            }
            if !self.eat(",") {
                break;
            }
            if all {
                return Err(self.error("`export *::*;` cannot have additional items"));
            }
        }
        self.expect(";")?;
        let id = PackageBindingId(self.package_bindings.len());
        self.package_bindings.push(PackageBinding {
            kind,
            items,
            tokens: self.range(start),
        });
        Ok(id)
    }
}
