//! The call sites an elaboration allocated, which a repeated one must allocate
//! anew when its items retain them.
//!
//! The patterns name every field, so a new field holding expressions is not
//! skipped silently.
use super::*;

/// Visits each call site of a value.
pub(in crate::ast) trait CallSites {
    fn call_sites(&self, visit: &mut dyn FnMut(usize));
}

impl<T: CallSites> CallSites for Vec<T> {
    fn call_sites(&self, visit: &mut dyn FnMut(usize)) {
        for value in self {
            value.call_sites(visit);
        }
    }
}

impl<T: CallSites> CallSites for Option<T> {
    fn call_sites(&self, visit: &mut dyn FnMut(usize)) {
        if let Some(value) = self {
            value.call_sites(visit);
        }
    }
}

impl<T: CallSites> CallSites for Box<T> {
    fn call_sites(&self, visit: &mut dyn FnMut(usize)) {
        (**self).call_sites(visit);
    }
}

impl CallSites for ConstExpr {
    fn call_sites(&self, visit: &mut dyn FnMut(usize)) {
        match self {
            ConstExpr::Literal(_) | ConstExpr::Ident(_) => {}
            ConstExpr::Select { expr, bit } => {
                expr.call_sites(visit);
                bit.call_sites(visit);
            }
            ConstExpr::SelectRange {
                expr,
                msb,
                lsb,
                signed: _,
            } => {
                expr.call_sites(visit);
                msb.call_sites(visit);
                lsb.call_sites(visit);
            }
            ConstExpr::Resize {
                expr,
                width: _,
                signed: _,
            } => expr.call_sites(visit),
            ConstExpr::Function {
                callee: _,
                args,
                site,
            } => {
                args.call_sites(visit);
                if let Some(site) = site {
                    visit(*site);
                }
            }
            ConstExpr::Unary { op: _, expr } => expr.call_sites(visit),
            ConstExpr::Binary { left, op: _, right } => {
                left.call_sites(visit);
                right.call_sites(visit);
            }
            ConstExpr::Mux {
                condition,
                then_expr,
                else_expr,
            } => {
                condition.call_sites(visit);
                then_expr.call_sites(visit);
                else_expr.call_sites(visit);
            }
        }
    }
}

impl CallSites for Expr {
    fn call_sites(&self, visit: &mut dyn FnMut(usize)) {
        match self {
            Expr::Ident(_) | Expr::Literal(_) => {}
            Expr::Select {
                expr,
                msb,
                lsb,
                signed: _,
            } => {
                expr.call_sites(visit);
                msb.call_sites(visit);
                lsb.call_sites(visit);
            }
            Expr::Concat(parts) => parts.call_sites(visit),
            Expr::RepeatConcat { count, parts } => {
                count.call_sites(visit);
                parts.call_sites(visit);
            }
            Expr::Resize {
                expr,
                width: _,
                signed: _,
            }
            | Expr::Unary { op: _, expr } => expr.call_sites(visit),
            Expr::Binary { left, op: _, right } => {
                left.call_sites(visit);
                right.call_sites(visit);
            }
            Expr::Mux {
                condition,
                then_expr,
                else_expr,
            } => {
                condition.call_sites(visit);
                then_expr.call_sites(visit);
                else_expr.call_sites(visit);
            }
            Expr::Call {
                callee: _,
                args,
                arg_order: _,
            } => args.call_sites(visit),
            Expr::Inside { expr, items } => {
                expr.call_sites(visit);
                items.call_sites(visit);
            }
        }
    }
}

impl CallSites for InsideItem {
    fn call_sites(&self, visit: &mut dyn FnMut(usize)) {
        match self {
            InsideItem::Value(value) => value.call_sites(visit),
            InsideItem::Range { low, high } => {
                low.call_sites(visit);
                high.call_sites(visit);
            }
        }
    }
}

impl CallSites for Type {
    fn call_sites(&self, visit: &mut dyn FnMut(usize)) {
        let Type {
            is_enum: _,
            enum_domain: _,
            packed_enum: _,
            kind: _,
            is_signed: _,
            packed_ranges,
            unpacked_ranges,
            members,
            signed_element_depth: _,
            class: _,
            virtual_interface: _,
            container: _,
            unpacked_struct: _,
            spelling: _,
        } = self;
        packed_ranges.call_sites(visit);
        unpacked_ranges.call_sites(visit);
        members.call_sites(visit);
    }
}

impl CallSites for PackedRange {
    fn call_sites(&self, visit: &mut dyn FnMut(usize)) {
        let PackedRange { left, right } = self;
        left.call_sites(visit);
        right.call_sites(visit);
    }
}

impl CallSites for UnpackedRange {
    fn call_sites(&self, visit: &mut dyn FnMut(usize)) {
        let UnpackedRange { left, right, size } = self;
        left.call_sites(visit);
        right.call_sites(visit);
        size.call_sites(visit);
    }
}

impl CallSites for VariableDimensions {
    fn call_sites(&self, visit: &mut dyn FnMut(usize)) {
        let VariableDimensions {
            declared: _,
            container: _,
            randomizable: _,
            enum_domain: _,
            packed_enum: _,
            real: _,
            string: _,
            class: _,
            virtual_interface: _,
            packed,
            unpacked,
            signed: _,
            is_2state: _,
            members,
            signed_element_depth: _,
            unpacked_struct: _,
        } = self;
        packed.call_sites(visit);
        unpacked.call_sites(visit);
        members.call_sites(visit);
    }
}

impl CallSites for PackedDimension {
    fn call_sites(&self, visit: &mut dyn FnMut(usize)) {
        let PackedDimension {
            left,
            right,
            width,
            normalize_single: _,
        } = self;
        left.call_sites(visit);
        right.call_sites(visit);
        width.call_sites(visit);
    }
}

impl CallSites for UnpackedDimension {
    fn call_sites(&self, visit: &mut dyn FnMut(usize)) {
        let UnpackedDimension { left, right, width } = self;
        left.call_sites(visit);
        right.call_sites(visit);
        width.call_sites(visit);
    }
}

impl CallSites for Item {
    fn call_sites(&self, visit: &mut dyn FnMut(usize)) {
        let Item {
            state: _,
            id: _,
            env: _,
            literals,
            parameter_dimensions,
            qualified_types,
            names: _,
            imports: _,
            shadowed: _,
            scope: _,
        } = self;
        for value in literals.values() {
            value.call_sites(visit);
        }
        for dimensions in parameter_dimensions.values() {
            dimensions.call_sites(visit);
        }
        for r#type in qualified_types.values() {
            r#type.call_sites(visit);
        }
    }
}

/// Whether `items` refer to a site in `sites`.
pub(super) fn refer_to(items: &[Item], sites: std::ops::Range<usize>) -> bool {
    let mut found = false;
    for item in items {
        item.call_sites(&mut |site| found |= sites.contains(&site));
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_sites_in_select_bounds_and_repeat_counts() {
        let call = |site| ConstExpr::Function {
            callee: Callee::Subroutine("f".into()),
            args: vec![ConstExpr::Literal("1".into())],
            site: Some(site),
        };
        let expr = Expr::RepeatConcat {
            count: call(3),
            parts: vec![Expr::Select {
                expr: Box::new(Expr::Ident("a".into())),
                msb: ConstExpr::Binary {
                    left: Box::new(call(7)),
                    op: BinaryOp::Add,
                    right: Box::new(ConstExpr::Literal("1".into())),
                },
                lsb: ConstExpr::Literal("0".into()),
                signed: false,
            }],
        };
        let mut sites = Vec::new();
        expr.call_sites(&mut |site| sites.push(site));
        assert_eq!(sites, [3, 7]);
    }
}
