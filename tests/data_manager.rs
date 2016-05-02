// Copyright 2016 MaidSafe.net limited.
//
// This SAFE Network Software is licensed to you under (1) the MaidSafe.net Commercial License,
// version 1.0 or later, or (2) The General Public License (GPL), version 3, depending on which
// licence you accepted on initial access to the Software (the "Licences").
//
// By contributing code to the SAFE Network Software, or to this project generally, you agree to be
// bound by the terms of the MaidSafe Contributor Agreement, version 1.0.  This, along with the
// Licenses can be found in the root directory of this project at LICENSE, COPYING and CONTRIBUTOR.
//
// Unless required by applicable law or agreed to in writing, the SAFE Network Software distributed
// under the GPL Licence is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied.
//
// Please review the Licences for the specific language governing permissions and limitations
// relating to use of the SAFE Network Software.

// For explanation of lint checks, run `rustc -W help` or see
// https://github.com/maidsafe/QA/blob/master/Documentation/Rust%20Lint%20Checks.md
#![forbid(bad_style, exceeding_bitshifts, mutable_transmutes, no_mangle_const_items,
          unknown_crate_types, warnings)]
#![deny(deprecated, drop_with_repr_extern, improper_ctypes, missing_docs,
        non_shorthand_field_patterns, overflowing_literals, plugin_as_library,
        private_no_mangle_fns, private_no_mangle_statics, stable_features, unconditional_recursion,
        unknown_lints, unsafe_code, unused, unused_allocation, unused_attributes,
        unused_comparisons, unused_features, unused_parens, while_true)]
#![warn(trivial_casts, trivial_numeric_casts, unused_extern_crates, unused_import_braces,
        unused_qualifications, unused_results)]
#![allow(box_pointers, fat_ptr_transmutes, missing_copy_implementations,
         missing_debug_implementations, variant_size_differences)]

#![cfg_attr(feature="clippy", feature(plugin))]
#![cfg_attr(feature="clippy", plugin(clippy))]
#![cfg_attr(feature="clippy", deny(clippy))]

#![cfg(test)]
#![cfg(feature = "use-mock-crust")]

extern crate kademlia_routing_table;
#[macro_use]
extern crate log;
#[macro_use]
#[allow(unused_extern_crates)]  // Only using macros from maidsafe_utilites
extern crate maidsafe_utilities;
extern crate rand;
extern crate routing;
extern crate safe_vault;

use std::cmp;

use kademlia_routing_table::GROUP_SIZE;
use rand::{random, thread_rng};
use rand::distributions::{IndependentSample, Range};
use routing::{Data, ImmutableData, StructuredData};
use routing::mock_crust::{self, Network};
use safe_vault::mock_crust_detail::{self, poll, test_node};
use safe_vault::mock_crust_detail::test_client::TestClient;
use safe_vault::test_utils;

const TEST_NET_SIZE: usize = GROUP_SIZE + 2; // just larger than CLOSE_GROUP

#[cfg(feature = "use-mock-crust")]
#[test]
fn immutable_data_churn() {
    let network = Network::new();
    let node_count = TEST_NET_SIZE;
    let mut nodes = test_node::create_nodes(&network, node_count, None);
    let config = mock_crust::Config::with_contacts(&[nodes[0].endpoint()]);
    let mut client = TestClient::new(&network, Some(config));
    const DATA_COUNT: usize = 5;
    const DATA_PER_ITER: usize = 2;

    client.ensure_connected(&mut nodes);
    client.create_account(&mut nodes);

    let mut all_data = vec![];
    let mut rng = thread_rng();

    for i in 0..10 {
        for _ in 0..(cmp::min(DATA_PER_ITER, DATA_COUNT - all_data.len())) {
            let data = Data::Immutable(ImmutableData::new(test_utils::generate_random_vec_u8(10)));
            trace!("Putting data {:?}.", data.name());
            client.put(data.clone());
            all_data.push(data);
        }
        trace!("Churning on {} nodes, iteration {}", nodes.len(), i);
        if nodes.len() <= GROUP_SIZE + 2 || random() {
            let index = Range::new(1, nodes.len()).ind_sample(&mut rng);
            trace!("Adding node with bootstrap node {}.", index);
            test_node::add_node(&network, &mut nodes, index);
        } else {
            let number = Range::new(3, 4).ind_sample(&mut rng);
            trace!("Removing {} node(s).", number);
            for _ in 0..number {
                let node_range = Range::new(1, nodes.len());
                let node_index = node_range.ind_sample(&mut rng);
                test_node::drop_node(&mut nodes, node_index);
            }
        }
        let _ = poll::nodes_and_client(&mut nodes, &mut client);

        mock_crust_detail::check_data(all_data.clone(), &nodes);
    }

    for data in &all_data {
        match *data {
            Data::Immutable(ref sent_data) => {
                match client.get(sent_data.identifier(), &mut nodes) {
                    Data::Immutable(recovered_data) => {
                        assert_eq!(recovered_data, *sent_data);
                    }
                    unexpected_data => panic!("Got unexpected data: {:?}", unexpected_data),
                }
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn structured_data_churn() {
    let network = Network::new();
    let node_count = TEST_NET_SIZE;
    let mut nodes = test_node::create_nodes(&network, node_count, None);
    let config = mock_crust::Config::with_contacts(&[nodes[0].endpoint()]);
    let mut client = TestClient::new(&network, Some(config));

    client.ensure_connected(&mut nodes);
    client.create_account(&mut nodes);

    let mut all_data: Vec<Data> = vec![];
    let mut deleted_data = vec![];
    let mut rng = thread_rng();
    let mut event_count = 0;

    for i in 0..10 {
        let mut new_data = vec![];
        for _ in 0..4 {
            if all_data.is_empty() || random() {
                let data = Data::Structured(test_utils::random_structured_data(100000,
                                                                               client.full_id()));
                trace!("Putting data {:?} with name {:?}.",
                       data.identifier(),
                       data.name());
                client.put(data.clone());
                new_data.push(data);
            } else {
                let j = Range::new(0, all_data.len()).ind_sample(&mut rng);
                let data = Data::Structured(if let Data::Structured(sd) = all_data[j].clone() {
                    unwrap_result!(StructuredData::new(sd.get_type_tag(),
                                                       *sd.get_identifier(),
                                                       sd.get_version() + 1,
                                                       test_utils::generate_random_vec_u8(10),
                                                       sd.get_owner_keys().clone(),
                                                       vec![],
                                                       Some(client.full_id()
                                                                  .signing_private_key())))
                } else {
                    panic!("Non-structured data found.");
                });
                // FIXME: Fix the delete-while-churn scenario and re-enable this.
                if false && Range::new(0, 3).ind_sample(&mut rng) == 0 {
                    trace!("Deleting data {:?} with name {:?}",
                           data.identifier(),
                           data.name());
                    client.delete(data);
                    deleted_data.push(all_data.remove(j));
                } else {
                    trace!("Posting data {:?} with name {:?}.",
                           data.identifier(),
                           data.name());
                    all_data[j] = data.clone();
                    client.post(data);
                }
            }
        }
        all_data.extend(new_data);
        trace!("Churning on {} nodes, iteration {}", nodes.len(), i);
        if nodes.len() <= GROUP_SIZE + 2 || random() {
            let index = Range::new(1, nodes.len()).ind_sample(&mut rng);
            trace!("Adding node with bootstrap node {}.", index);
            test_node::add_node(&network, &mut nodes, index);
        } else {
            let number = Range::new(3, 4).ind_sample(&mut rng);
            trace!("Removing {} node(s).", number);
            for _ in 0..number {
                let node_range = Range::new(1, nodes.len());
                let node_index = node_range.ind_sample(&mut rng);
                test_node::drop_node(&mut nodes, node_index);
            }
        }
        let count = poll::nodes_and_client(&mut nodes, &mut client);
        trace!("Processed {} events.", count);
        event_count += count;

        mock_crust_detail::check_data(all_data.clone(), &nodes);
        mock_crust_detail::check_deleted_data(&deleted_data, &nodes);
    }

    for data in &all_data {
        match *data {
            Data::Structured(ref sent_structured_data) => {
                match client.get(sent_structured_data.identifier(), &mut nodes) {
                    Data::Structured(recovered_structured_data) => {
                        assert_eq!(recovered_structured_data, *sent_structured_data);
                    }
                    unexpected_data => panic!("Got unexpected data: {:?}", unexpected_data),
                }
            }
            _ => unreachable!(),
        }
    }

    trace!("Processed {} events.", event_count);
}


#[test]
fn handle_put_get_normal_flow() {}

#[test]
fn handle_put_get_error_flow() {}

#[test]
fn handle_post() {}

#[test]
fn handle_delete() {}

#[test]
fn get_non_existing_data() {}

#[test]
fn get_existing_data() {}
