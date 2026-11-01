//! Native foreach slots feed the shared dimension and lexical declaration kernel.
use super::*;
use celox_sv_syntax::{ExprKind, ForeachHeader};

pub(super) enum ForeachLoop {
    Static(String, i128, i128),
    Container {
        array: Expr,
        index: String,
        layout: ContainerLayout,
        status: Option<String>,
    },
    /// The characters of a string, indexed from 0 below its length.
    String {
        array: Expr,
        index: String,
    },
}

impl BodyBuilder<'_, '_, '_> {
    pub(super) fn native_foreach_indices(
        &mut self,
        header: &ForeachHeader,
    ) -> Result<Vec<ForeachLoop>, AnalyzerError> {
        let native = self.tree.declarations();
        let container =
            containers::native_type(header.array, native, self.tree, self.session, &self.dims)?;
        let mut array = header.array;
        let mut unit_member = None;
        let array = loop {
            match &native.expression(array).kind {
                ExprKind::Name(name) if name.spelling == "$unit" => {
                    break unit_member.ok_or_else(|| unsupported("foreach array"))?;
                }
                ExprKind::Name(name) => break instances::normalize_identifier(&name.spelling),
                ExprKind::Member {
                    expression, name, ..
                } => {
                    unit_member = Some(instances::normalize_identifier(&name.spelling));
                    array = *expression;
                }
                ExprKind::Select { expression, .. } => array = *expression,
                _ => return Err(unsupported("foreach array")),
            }
        };
        if let Some(container) = container {
            // Each loop variable iterates one dimension, the first the
            // outermost; the element at the outer indices is the container
            // the next one iterates (IEEE 1800-2023 12.7.3). A skipped
            // variable iterates its dimension without a name when a later
            // one is named, and trailing skipped ones do not iterate.
            let named = header
                .variables
                .iter()
                .rposition(Option::is_some)
                .ok_or_else(|| unsupported("container foreach without an index"))?;
            let mut array = self.native_value(header.array)?;
            let mut container = container;
            let mut loops = Vec::new();
            for (position, variable) in header.variables[..=named].iter().enumerate() {
                if position > 0 {
                    let outer = container.layout;
                    let ForeachLoop::Container { index, .. } = loops.last().expect("an outer loop")
                    else {
                        unreachable!("container loops");
                    };
                    array = containers::call(
                        ContainerOp::Get,
                        outer,
                        vec![array, Expr::Ident(index.clone())],
                    );
                    container = container.element.container.clone().ok_or_else(|| {
                        unsupported("foreach index beyond the container dimensions")
                    })?;
                }
                let (index_type, status) = self.container_index_type(&container);
                let index = match variable {
                    Some(variable) => self.declare(
                        &instances::normalize_identifier(&variable.spelling),
                        index_type,
                    ),
                    None => self.declare("$celox_foreach_index", index_type),
                };
                loops.push(ForeachLoop::Container {
                    array: array.clone(),
                    index,
                    layout: container.layout,
                    status,
                });
            }
            return Ok(loops);
        }
        // A string is a dynamic array of bytes indexed from 0 to N-1, where
        // N is its length (IEEE 1800-2023 12.7.3).
        if expression_binding::native_string(
            self.tree,
            self.session,
            header.array,
            native,
            &self.dims,
        ) {
            let [variable] = &header.variables[..] else {
                return Err(unsupported(
                    "foreach over a string with more than one index",
                ));
            };
            // An omitted index skips the string's only dimension, so the
            // body runs once (12.7.3).
            let Some(variable) = variable else {
                return Ok(Vec::new());
            };
            let array = self.native_value(header.array)?;
            let index = self.declare(
                &instances::normalize_identifier(&variable.spelling),
                int_type(),
            );
            return Ok(vec![ForeachLoop::String { array, index }]);
        }
        let variables = header
            .variables
            .iter()
            .map(|name| {
                name.as_ref()
                    .map(|name| instances::normalize_identifier(&name.spelling))
            })
            .collect();
        self.foreach_bound_indices(array, variables)
    }

    /// The type of the loop variable that iterates `container`, and for an
    /// associative array the status local of its traversal.
    fn container_index_type(&mut self, container: &ContainerType) -> (Type, Option<String>) {
        let ContainerKind::Associative {
            key_width,
            signed,
            string,
            object,
        } = container.layout.kind
        else {
            return (int_type(), None);
        };
        // A process key keeps its type, so the index is a handle whose
        // methods the body may call.
        let process = container
            .key
            .as_ref()
            .filter(|key| key.kind() == TypeKind::Process);
        let index_type = if object || container.key.as_ref().is_some_and(|key| key.chandle) {
            container
                .key
                .clone()
                .expect("a class or chandle index type")
        } else if let Some(process) = process {
            process.clone()
        } else if string {
            type_binding::builtin_type("string", None).expect("string is a builtin type")
        } else if let Some(key) = container
            .key
            .as_ref()
            .filter(|key| !key.members.is_empty() || key.is_enum)
        {
            // A packed structure or enumeration index keeps its type, so the
            // body may select its members or call its methods (IEEE
            // 1800-2023 12.7.3, 6.19.5).
            key.clone()
        } else {
            let mut ty = int_type();
            ty.is_signed = signed;
            ty.packed_ranges[0].left = ConstExpr::Literal((key_width - 1).to_string());
            ty
        };
        (
            index_type,
            Some(self.declare("$celox_foreach_status", int_type())),
        )
    }
}
