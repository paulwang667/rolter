-- allow routes and provider groups to select the fastest_share strategy.
-- it spreads picks by latency share rather than sending each one to the single
-- fastest target. Widening the constraint moves no traffic on its own: a route
-- changes only when an operator selects the new value.
alter table routes drop constraint if exists routes_strategy_check;
alter table routes
    add constraint routes_strategy_check
    check (strategy in (
        'round_robin', 'random', 'power_of_two', 'consistent_hash',
        'cache_aware', 'weighted', 'pipeline', 'cheapest', 'fastest',
        'precise_cache_aware', 'lmcache_aware', 'adaptive', 'lora_aware',
        'predicted_latency', 'fastest_share'
    ));

alter table provider_groups drop constraint if exists provider_groups_strategy_check;
alter table provider_groups
    add constraint provider_groups_strategy_check
    check (strategy in (
        'round_robin', 'random', 'power_of_two', 'consistent_hash',
        'cache_aware', 'weighted', 'pipeline', 'cheapest', 'fastest',
        'precise_cache_aware', 'lmcache_aware', 'adaptive', 'lora_aware',
        'predicted_latency', 'fastest_share'
    ));
