# Copyright 2024 The Tari Project
# SPDX-License-Identifier: BSD-3-Clause

@sync
Feature: Sync p2pool nodes

  @critical
  Scenario: New node should sync with peers and propagate blocks
    Given I have a base node BASE_NODE_A
    And I have a p2pool seed node SEED in squad WINNERS connected to base node BASE_NODE_A
    And I have a p2pool node NODE_A in squad WINNERS connected to base node BASE_NODE_A
    And I have a p2pool node NODE_B in squad WINNERS connected to base node BASE_NODE_A
    And p2pool node NODE_A stats shows connected to peer NODE_B
    And I add 10 blocks to p2pool node NODE_A
    And p2pool node NODE_A stats is at height 10
    And p2pool node NODE_B stats is at height 10
    # Add new node, also syncs
    And I have a p2pool node NODE_C in squad WINNERS connected to base node BASE_NODE_A
    And p2pool node NODE_A stats shows connected to peer NODE_C
    And p2pool node NODE_B stats shows connected to peer NODE_C
    And p2pool node NODE_C stats is at height 10
    # Mine blocks on new node, all nodes sync
    And I add 10 blocks to p2pool node NODE_C
    And p2pool node NODE_A stats is at height 20
    And p2pool node NODE_B stats is at height 20
    And p2pool node NODE_C stats is at height 20
    Then I wait 1 seconds and stop

  @critical
  Scenario: Different squads should stay on their respective chains
    # WINNERS
    Given I have a base node BASE_NODE_A
    And I have a p2pool seed node SEED_A in squad WINNERS connected to base node BASE_NODE_A
    And I have a p2pool node NODE_A1 in squad WINNERS connected to base node BASE_NODE_A
    And I have a p2pool node NODE_A2 in squad WINNERS connected to base node BASE_NODE_A
    # LOSERS
    Given I have a base node BASE_NODE_B
    And I have a p2pool seed node SEED_B in squad LOSERS connected to base node BASE_NODE_B
    And I have a p2pool node NODE_B1 in squad LOSERS connected to base node BASE_NODE_B
    And I have a p2pool node NODE_B2 in squad LOSERS connected to base node BASE_NODE_B
    # WINNERS connected to WINNERS
    And p2pool node NODE_A1 stats shows connected to peer NODE_A2
    # LOSERS connected to LOSERS
    And p2pool node NODE_B1 stats shows connected to peer NODE_B2
    # WINNERS mine and sync
    And I add 10 blocks to p2pool node NODE_A1
    And p2pool node NODE_A1 stats is at height 10
    And p2pool node NODE_A2 stats is at height 10
    # LOSERS mine and sync
    And I add 5 blocks to p2pool node NODE_B1
    And p2pool node NODE_B1 stats is at height 5
    And p2pool node NODE_B2 stats is at height 5
    # WINNERS mine and sync some more
    And I add 5 blocks to p2pool node NODE_A1
    And p2pool node NODE_A1 stats is at height 15
    And p2pool node NODE_A2 stats is at height 15
    # LOSERS stay on their chain
    And p2pool node NODE_B1 stats is at height 5
    And p2pool node NODE_B2 stats is at height 5
    Then I wait 1 seconds and stop
