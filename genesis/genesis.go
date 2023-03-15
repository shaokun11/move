// Copyright (C) 2023, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

package genesis

import (
	"context"
	"encoding/json"
	"fmt"

	"github.com/ava-labs/avalanchego/trace"
	"github.com/ava-labs/hypersdk/chain"
	hconsts "github.com/ava-labs/hypersdk/consts"
	"github.com/ava-labs/hypersdk/vm"

	"github.com/ava-labs/indexvm/consts"
	"github.com/ava-labs/indexvm/storage"
	"github.com/ava-labs/indexvm/utils"
)

var _ vm.Genesis = (*Genesis)(nil)

type CustomAllocation struct {
	Address string `json:"address"` // bech32 address
	Balance uint64 `json:"balance"`
}

type Genesis struct {
	// Address prefix
	HRP string `json:"hrp"`

	// Block params
	MaxBlockTxs   int    `json:"maxBlockTxs"`
	MaxBlockUnits uint64 `json:"maxBlockUnits"` // must be possible to reach before block too large

	// Tx params
	BaseUnits      uint64 `json:"baseUnits"`
	StateLockup    uint64 `json:"stateLockup"`    // cost per key added to state
	ValidityWindow int64  `json:"validityWindow"` // seconds

	// Unit pricing
	MinUnitPrice               uint64 `json:"minUnitPrice"`
	UnitPriceChangeDenominator uint64 `json:"unitPriceChangeDenominator"`
	WindowTargetUnits          uint64 `json:"windowTargetUnits"` // 10s

	// Block pricing
	MinBlockCost               uint64 `json:"minBlockCost"`
	BlockCostChangeDenominator uint64 `json:"blockCostChangeDenominator"`
	WindowTargetBlocks         uint64 `json:"windowTargetBlocks"` // 10s

	// Allocations
	CustomAllocation []*CustomAllocation `json:"customAllocation"`
}

func Default() *Genesis {
	return &Genesis{
		HRP: consts.HRP,

		// Block params
		MaxBlockTxs:   20_000,    // rely on max block units
		MaxBlockUnits: 1_800_000, // 1.8 MiB

		// Tx params
		BaseUnits:      48, // timestamp(8) + chainID(32) + unitPrice(8)
		StateLockup:    1_024,
		ValidityWindow: 60,

		// Unit Pricing
		MinUnitPrice:               1,
		UnitPriceChangeDenominator: 48,
		WindowTargetUnits:          9_000_000, // 9 MiB

		// Block pricing
		MinBlockCost:               0,
		BlockCostChangeDenominator: 48,
		WindowTargetBlocks:         20, // 10s
	}
}

func New(b []byte, _ []byte /* upgradeBytes */) (*Genesis, error) {
	g := Default()
	if len(b) > 0 {
		if err := json.Unmarshal(b, g); err != nil {
			return nil, fmt.Errorf("failed to unmarshal config %s: %w", string(b), err)
		}
	}
	if g.WindowTargetUnits == 0 {
		return nil, ErrInvalidTarget
	}
	if g.WindowTargetBlocks == 0 {
		return nil, ErrInvalidTarget
	}
	return g, nil
}

func (g *Genesis) GetHRP() string {
	return g.HRP
}

func (g *Genesis) Load(ctx context.Context, tracer trace.Tracer, db chain.Database) error {
	ctx, span := tracer.Start(ctx, "Genesis.Load")
	defer span.End()

	for _, alloc := range g.CustomAllocation {
		pk, err := utils.ParseAddress(alloc.Address)
		if err != nil {
			return err
		}
		if _, err := storage.AddUnlockedBalance(ctx, db, pk, alloc.Balance, false); err != nil {
			return fmt.Errorf("%w: addr=%s, bal=%d", err, alloc.Address, alloc.Balance)
		}
		if err := storage.LockBalance(ctx, db, pk, g.StateLockup*2); err != nil {
			return err
		}
		if err := storage.SetPermissions(ctx, db, pk, pk, hconsts.MaxUint8, hconsts.MaxUint8); err != nil {
			return err
		}
	}
	return nil
}
