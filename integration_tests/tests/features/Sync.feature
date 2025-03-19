# Copyright 2024 The Tari Project
# SPDX-License-Identifier: BSD-3-Clause

@sync
Feature: Sync p2pool nodes

  @critical
  Scenario: New node sync with peers on startup
    Given I have a base node BASE_NODE_A
    And I have a p2pool seed node SEED in squad DOLPHINS connected to base node BASE_NODE_A
    And I have a p2pool seed node SEED2 in squad DOLPHINS connected to base node BASE_NODE_A
    And I have a p2pool node NODE_A in squad DOLPHINS connected to base node BASE_NODE_A
    And I add 10 blocks to p2pool node NODE_A
    And p2pool node NODE_A stats is at height 10
    # Add new nodes, they sync
    And I have a p2pool node NODE_B in squad DOLPHINS connected to base node BASE_NODE_A
    And I have a p2pool node NODE_C in squad DOLPHINS connected to base node BASE_NODE_A
    And I have a p2pool node NODE_D in squad DOLPHINS connected to base node BASE_NODE_A
    And p2pool node NODE_D stats shows connected to peer NODE_A
    And p2pool node NODE_D stats shows connected to peer NODE_B
    And p2pool node NODE_D stats shows connected to peer NODE_C
    And p2pool node NODE_D stats is at height 10
    Then I wait 1 seconds and stop

@critical
  Scenario: Node will load up blocks from storage on startup
    Given I have a base node BASE_NODE_A
    And I have a p2pool seed node SEED in squad DOLPHINS connected to base node BASE_NODE_A
    And I have a p2pool node NODE_A in squad DOLPHINS connected to base node BASE_NODE_A
    And I add 10 blocks to p2pool node NODE_A
    And p2pool node NODE_A stats is at height 10
    # Stop the node
    And I stop p2pool node NODE_A
    # Start-up node again, it loads blocks from storage
    And I re-start p2pool node NODE_A
    And p2pool node NODE_A stats is at height 10

  @critical
  Scenario: New node can be offline and then sync with peers
    Given I have a base node BASE_NODE_A
    And I have a p2pool seed node SEED in squad DOLPHINS connected to base node BASE_NODE_A
    And I have a p2pool node NODE_A in squad DOLPHINS connected to base node BASE_NODE_A
    And I add 10 blocks to p2pool node NODE_A
    And p2pool node NODE_A stats is at height 10
    # Add new node, it syncs
    And I have a p2pool node NODE_B in squad DOLPHINS connected to base node BASE_NODE_A
    And p2pool node NODE_A stats shows connected to peer NODE_B
    And p2pool node NODE_B stats is at height 10
    # Stop new node
    And I stop p2pool node NODE_B
    # Mine some more blocks on NODE_A
    And I add 10 blocks to p2pool node NODE_A
    And p2pool node NODE_A stats is at height 20
    # Start-up the new node again, it syncs
    And I re-start p2pool node NODE_B
    And p2pool node NODE_A stats shows connected to peer NODE_B
    And p2pool node NODE_B stats is at height 20
    Then I wait 1 seconds and stop

  @critical
  Scenario: New node participate in block propagation
    Given I have a base node BASE_NODE_A
    And I have a p2pool seed node SEED in squad DOLPHINS connected to base node BASE_NODE_A
    And I have a p2pool node NODE_A in squad DOLPHINS connected to base node BASE_NODE_A
    And I have a p2pool node NODE_B in squad DOLPHINS connected to base node BASE_NODE_A
    And p2pool node NODE_A stats shows connected to peer NODE_B
    And I add 10 blocks to p2pool node NODE_A
    And p2pool node NODE_A stats is at height 10
    And p2pool node NODE_B stats is at height 10
    # Add new node, also syncs
    And I have a p2pool node NODE_C in squad DOLPHINS connected to base node BASE_NODE_A
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
  Scenario: Different squads stay on their respective chains
    # DOLPHINS
    Given I have a base node BASE_NODE_A
    And I have a p2pool seed node SEED_A in squad DOLPHINS connected to base node BASE_NODE_A
    And I have a p2pool node NODE_A1 in squad DOLPHINS connected to base node BASE_NODE_A
    And I have a p2pool node NODE_A2 in squad DOLPHINS connected to base node BASE_NODE_A
    # TURTLES
    Given I have a base node BASE_NODE_B
    And I have a p2pool seed node SEED_B in squad TURTLES connected to base node BASE_NODE_B
    And I have a p2pool node NODE_B1 in squad TURTLES connected to base node BASE_NODE_B
    And I have a p2pool node NODE_B2 in squad TURTLES connected to base node BASE_NODE_B
    # DOLPHINS connected to DOLPHINS
    And p2pool node NODE_A1 stats shows connected to peer NODE_A2
    # TURTLES connected to TURTLES
    And p2pool node NODE_B1 stats shows connected to peer NODE_B2
    # DOLPHINS mine and sync
    And I add 10 blocks to p2pool node NODE_A1
    And p2pool node NODE_A1 stats is at height 10
    And p2pool node NODE_A2 stats is at height 10
    # TURTLES mine and sync
    And I add 5 blocks to p2pool node NODE_B1
    And p2pool node NODE_B1 stats is at height 5
    And p2pool node NODE_B2 stats is at height 5
    # DOLPHINS mine and sync some more
    And I add 5 blocks to p2pool node NODE_A1
    And p2pool node NODE_A1 stats is at height 15
    And p2pool node NODE_A2 stats is at height 15
    # TURTLES stay on their chain
    And p2pool node NODE_B1 stats is at height 5
    And p2pool node NODE_B2 stats is at height 5
    Then I wait 1 seconds and stop

@critical
  Scenario: I run a diagnostic node to learn about the network connections
    Given I have a base node BASE_NODE_A
    # We have 3 seed nodes
    And I have a p2pool seed node SEED_A in squad DOLPHINS connected to base node BASE_NODE_A
    And I have a p2pool seed node SEED_B in squad DOLPHINS connected to base node BASE_NODE_A
    And I have a p2pool seed node SEED_C in squad DOLPHINS connected to base node BASE_NODE_A
    # We have 10 normal nodes
    And I have a p2pool node NODE_A in squad DOLPHINS connected to base node BASE_NODE_A
    And I have a p2pool node NODE_B in squad DOLPHINS connected to base node BASE_NODE_A
    And I have a p2pool node NODE_C in squad DOLPHINS connected to base node BASE_NODE_A
    And I have a p2pool node NODE_D in squad DOLPHINS connected to base node BASE_NODE_A
    And I have a p2pool node NODE_E in squad DOLPHINS connected to base node BASE_NODE_A
    And I have a p2pool node NODE_F in squad DOLPHINS connected to base node BASE_NODE_A
    And I have a p2pool node NODE_G in squad DOLPHINS connected to base node BASE_NODE_A
    And I have a p2pool node NODE_H in squad DOLPHINS connected to base node BASE_NODE_A
    And I have a p2pool node NODE_I in squad DOLPHINS connected to base node BASE_NODE_A
    # The network is well connected
    And p2pool node NODE_A stats shows connected to peer NODE_B
    And p2pool node NODE_B stats shows connected to peer NODE_C
    And p2pool node NODE_C stats shows connected to peer NODE_D
    And p2pool node NODE_D stats shows connected to peer NODE_E
    And p2pool node NODE_E stats shows connected to peer NODE_F
    And p2pool node NODE_F stats shows connected to peer NODE_G
    And p2pool node NODE_G stats shows connected to peer NODE_H
    And p2pool node NODE_H stats shows connected to peer NODE_I
    And p2pool node NODE_I stats shows connected to peer NODE_A
    # We start a diagnostic node to learn about the network
    And I have a p2pool diagnostic node DIAGNOSTIC in squad DOLPHINS connected to base node BASE_NODE_A
    And I wait up to 120 seconds for p2pool diagnostic node DIAGNOSTIC to create a diagnostic file
    Then I wait 1 seconds and stop