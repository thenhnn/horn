#
# Copyright 2025, Colias Group, LLC
#
# SPDX-License-Identifier: BSD-2-Clause
#

{ mk, localCrates }:

mk {
  package.name = "sddf";
  dependencies = {
    inherit (localCrates)
      sddf-sys
    ;
  };
}
