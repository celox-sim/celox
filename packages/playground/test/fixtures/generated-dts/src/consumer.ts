// Type-checked against the generated sidecars; never executed.
import { type FourStateSignalValue, X } from "@celox-sim/celox";
import {
	Consumer,
	type ConsumerPorts,
	Producer,
	type ProducerPorts,
} from "./Buses.veryl";
import {
	Leaf,
	type LeafPorts,
	Pair,
	type PairPorts,
	Single,
	type SinglePorts,
} from "./Multi.veryl";
import { Nested, type NestedPorts } from "./Nested.veryl";

declare const value: FourStateSignalValue;

export function exercise(
	leaf: LeafPorts,
	pair: PairPorts,
	single: SinglePorts,
	nested: NestedPorts,
	consumer: ConsumerPorts,
	producer: ProducerPorts,
): bigint[] {
	leaf.i_data = X;
	pair.rst = value;
	pair.flag = 1n;
	pair.top_in = 3n;
	single.top_in = 4n;
	nested.d = 5n;
	nested.blk.inner = X;
	nested.blk["blk2.deep"] = 6n;
	consumer.bus.data = 7n;
	consumer.mem.set(0, X);
	producer.inp = 8n;
	return [
		leaf.o_data,
		pair.top_out.at(1),
		pair.u_leaf[0]!.o_data,
		single.u_leaf.o_data,
		nested.q,
		nested.blk["blk2.deep"],
		consumer.out,
		consumer.mem.at(3),
		producer.bus.data,
		producer.bus.valid,
	];
}

export const modules = [Leaf, Pair, Single, Nested, Consumer, Producer];
