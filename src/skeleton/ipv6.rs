use ipnet::Ipv6Net;
use rand::{thread_rng, Rng};
use std::{collections::HashSet, net::Ipv6Addr};

pub fn random_address_in_cidr(net: Ipv6Net, excludes: &HashSet<Ipv6Addr>) -> Option<Ipv6Addr> {
    const MAX_TRIES: usize = 10_000;

    let prefix_len = net.prefix_len();
    let free_bits = 128 - prefix_len;

    if free_bits <= 1 {
        return None;
    }

    let network_bits = net.network().to_bits();
    let broadcast_addr = net.broadcast();

    let mut rng = thread_rng();
    for _ in 0..MAX_TRIES {
        let rand_host_bits = rng.gen_range(1..(1u128 << free_bits));
        let addr = Ipv6Addr::from_bits(network_bits | rand_host_bits);

        if addr != broadcast_addr && !excludes.contains(&addr) {
            return Some(addr);
        }
    }

    None
}
