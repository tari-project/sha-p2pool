#
# Copyright 2024 The Tari Project
# SPDX-License-Identifier: BSD-3-Clause
#

cargo +nightly ci-fmt && cargo machete && cargo ci-check && cargo ci-test && cargo ci-cucumber