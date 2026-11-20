// Copyright SimpleDevOps contributors. Licensed under Apache-2.0.

#pragma once

#include "CoreMinimal.h"
#include "ISourceControlProvider.h"
#include "Widgets/DeclarativeSyntaxSupport.h"
#include "Widgets/SCompoundWidget.h"
#include "Widgets/Views/SListView.h"

#include "LodeTypes.h"

/** The Lode Shelves window: every open shelf on the server, with its files; unshelve one here. */
class SLodeShelves : public SCompoundWidget
{
public:
	SLATE_BEGIN_ARGS(SLodeShelves) {}
	SLATE_END_ARGS()

	void Construct(const FArguments& InArgs);
	virtual ~SLodeShelves() override;

private:
	using FShelfPtr = TSharedPtr<FLodeShelfRecord>;

	FReply OnRefresh();
	FReply OnUnshelve();
	bool CanUnshelve() const;
	void OnListDone(const FSourceControlOperationRef& Operation, ECommandResult::Type Result);
	TSharedRef<ITableRow> MakeRow(FShelfPtr Item, const TSharedRef<STableViewBase>& Owner);
	void OnSelectionChanged(FShelfPtr Item, ESelectInfo::Type SelectInfo);
	FText GetDetails() const;
	FText GetStatus() const { return Status; }

	TArray<FShelfPtr> Shelves;
	TSharedPtr<SListView<FShelfPtr>> ListView;
	FShelfPtr Selected;
	FText Status;
	bool bBusy = false;
	TSharedPtr<bool, ESPMode::ThreadSafe> bAlive;
};
