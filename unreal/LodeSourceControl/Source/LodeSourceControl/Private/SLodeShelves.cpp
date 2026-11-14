// Copyright SimpleDevOps contributors. Licensed under Apache-2.0.

#include "SLodeShelves.h"

#include "ISourceControlModule.h"
#include "Misc/MessageDialog.h"
#include "Widgets/Input/SButton.h"
#include "Widgets/Layout/SScrollBox.h"
#include "Widgets/Layout/SSplitter.h"
#include "Widgets/SBoxPanel.h"
#include "Widgets/Text/STextBlock.h"
#include "Widgets/Views/SHeaderRow.h"

#include "LodeOperations.h"
#include "LodeProvider.h"
#include "LodeSourceControlModule.h"

#define LOCTEXT_NAMESPACE "LodeSourceControl.Shelves"

namespace
{
	const FName ColumnId(TEXT("Id"));
	const FName ColumnOwner(TEXT("Owner"));
	const FName ColumnDescription(TEXT("Description"));
	const FName ColumnBranch(TEXT("Branch"));
	const FName ColumnUpdated(TEXT("Updated"));
	const FName ColumnFiles(TEXT("Files"));

	class SShelfRow : public SMultiColumnTableRow<TSharedPtr<FLodeShelfRecord>>
	{
	public:
		SLATE_BEGIN_ARGS(SShelfRow) {}
		SLATE_END_ARGS()

		void Construct(const FArguments& InArgs, const TSharedRef<STableViewBase>& Owner, TSharedPtr<FLodeShelfRecord> InItem)
		{
			Item = InItem;
			SMultiColumnTableRow<TSharedPtr<FLodeShelfRecord>>::Construct(FSuperRowType::FArguments(), Owner);
		}

		virtual TSharedRef<SWidget> GenerateWidgetForColumn(const FName& Column) override
		{
			FText Text;
			if (Column == ColumnId) Text = FText::AsNumber(Item->Id);
			else if (Column == ColumnOwner) Text = FText::FromString(Item->Owner);
			else if (Column == ColumnDescription) Text = FText::FromString(Item->Description);
			else if (Column == ColumnBranch) Text = FText::FromString(Item->Branch);
			else if (Column == ColumnUpdated) Text = FText::AsDateTime(Item->UpdatedAt);
			else if (Column == ColumnFiles) Text = FText::AsNumber(Item->Filenames.Num());
			return SNew(STextBlock).Text(Text).Margin(FMargin(4.0f, 2.0f));
		}

	private:
		TSharedPtr<FLodeShelfRecord> Item;
	};
}

void SLodeShelves::Construct(const FArguments& InArgs)
{
	bAlive = MakeShared<bool, ESPMode::ThreadSafe>(true);

	ChildSlot
	[
		SNew(SVerticalBox)
		+ SVerticalBox::Slot()
		.AutoHeight()
		.Padding(4.0f)
		[
			SNew(SHorizontalBox)
			+ SHorizontalBox::Slot()
			.AutoWidth()
			[
				SNew(SButton)
				.Text(LOCTEXT("Refresh", "Refresh"))
				.IsEnabled_Lambda([this]() { return !bBusy; })
				.OnClicked(this, &SLodeShelves::OnRefresh)
			]
			+ SHorizontalBox::Slot()
			.AutoWidth()
			.Padding(4.0f, 0.0f)
			[
				SNew(SButton)
				.Text(LOCTEXT("Unshelve", "Unshelve Into My Workspace"))
				.ToolTipText(LOCTEXT("UnshelveTip", "Brings the shelf's changes into your workspace. Files you changed too are merged; binary assets changed on both sides become conflicts."))
				.IsEnabled(this, &SLodeShelves::CanUnshelve)
				.OnClicked(this, &SLodeShelves::OnUnshelve)
			]
			+ SHorizontalBox::Slot()
			.FillWidth(1.0f)
			.VAlign(VAlign_Center)
			.Padding(8.0f, 0.0f)
			[
				SNew(STextBlock)
				.Text(this, &SLodeShelves::GetStatus)
				.AutoWrapText(true)
			]
		]
		+ SVerticalBox::Slot()
		.FillHeight(1.0f)
		[
			SNew(SSplitter)
			.Orientation(Orient_Vertical)
			+ SSplitter::Slot()
			.Value(0.6f)
			[
				SAssignNew(ListView, SListView<FShelfPtr>)
				.ListItemsSource(&Shelves)
				.SelectionMode(ESelectionMode::Single)
				.OnGenerateRow(this, &SLodeShelves::MakeRow)
				.OnSelectionChanged(this, &SLodeShelves::OnSelectionChanged)
				.HeaderRow
				(
					SNew(SHeaderRow)
					+ SHeaderRow::Column(ColumnId).DefaultLabel(LOCTEXT("Id", "Shelf")).FillWidth(0.08f)
					+ SHeaderRow::Column(ColumnOwner).DefaultLabel(LOCTEXT("Owner", "Owner")).FillWidth(0.14f)
					+ SHeaderRow::Column(ColumnDescription).DefaultLabel(LOCTEXT("Description", "Description")).FillWidth(0.44f)
					+ SHeaderRow::Column(ColumnBranch).DefaultLabel(LOCTEXT("Branch", "Branch")).FillWidth(0.1f)
					+ SHeaderRow::Column(ColumnUpdated).DefaultLabel(LOCTEXT("Updated", "Updated")).FillWidth(0.16f)
					+ SHeaderRow::Column(ColumnFiles).DefaultLabel(LOCTEXT("Files", "Files")).FillWidth(0.08f)
				)
			]
			+ SSplitter::Slot()
			.Value(0.4f)
			[
				SNew(SScrollBox)
				+ SScrollBox::Slot()
				.Padding(4.0f)
				[
					SNew(STextBlock)
					.Text(this, &SLodeShelves::GetDetails)
					.AutoWrapText(true)
				]
			]
		]
	];

	OnRefresh();
}

SLodeShelves::~SLodeShelves()
{
	if (bAlive.IsValid())
	{
		*bAlive = false;
	}
}

TSharedRef<ITableRow> SLodeShelves::MakeRow(FShelfPtr Item, const TSharedRef<STableViewBase>& Owner)
{
	return SNew(SShelfRow, Owner, Item);
}

void SLodeShelves::OnSelectionChanged(FShelfPtr Item, ESelectInfo::Type SelectInfo)
{
	Selected = Item;
}

FText SLodeShelves::GetDetails() const
{
	if (!Selected.IsValid())
	{
		return LOCTEXT("SelectShelf", "Select a shelf to see its files.");
	}
	FString Text = FString::Printf(TEXT("Shelf %d, version %d, by %s on %s (from r%d)\n%s\n\n"),
		Selected->Id, Selected->Version, *Selected->Owner, *Selected->Branch, Selected->BaseRev, *Selected->Description);
	const FString Root = FLodeSourceControlModule::Get().GetProvider().GetWorkspace().Root;
	for (int32 Index = 0; Index < Selected->Filenames.Num(); ++Index)
	{
		FString Shown = Selected->Filenames[Index];
		FPaths::MakePathRelativeTo(Shown, *(Root + TEXT("/")));
		Text += FString::Printf(TEXT("%s  %s\n"), *Selected->Actions[Index], *Shown);
	}
	return FText::FromString(Text);
}

bool SLodeShelves::CanUnshelve() const
{
	return !bBusy && Selected.IsValid();
}

FReply SLodeShelves::OnRefresh()
{
	FLodeProvider& Provider = FLodeSourceControlModule::Get().GetProvider();
	if (!Provider.IsAvailable())
	{
		Status = LOCTEXT("NotConnected", "Lode is not connected. Choose Lode in Revision Control settings and sign in.");
		return FReply::Handled();
	}
	bBusy = true;
	Status = LOCTEXT("Loading", "Loading shelves...");
	TWeakPtr<bool, ESPMode::ThreadSafe> WeakAlive = bAlive;
	Provider.Execute(ISourceControlOperation::Create<FLodeListShelves>(), FSourceControlChangelistPtr(), TArray<FString>(), EConcurrency::Asynchronous,
		FSourceControlOperationComplete::CreateLambda([this, WeakAlive](const FSourceControlOperationRef& Operation, ECommandResult::Type Result)
		{
			const TSharedPtr<bool, ESPMode::ThreadSafe> Alive = WeakAlive.Pin();
			if (Alive.IsValid() && *Alive)
			{
				OnListDone(Operation, Result);
			}
		}));
	return FReply::Handled();
}

void SLodeShelves::OnListDone(const FSourceControlOperationRef& Operation, ECommandResult::Type Result)
{
	bBusy = false;
	Shelves.Reset();
	Selected.Reset();
	if (Result == ECommandResult::Succeeded)
	{
		for (const FLodeShelfRecord& Shelf : StaticCastSharedRef<FLodeListShelves>(Operation)->Shelves)
		{
			Shelves.Add(MakeShared<FLodeShelfRecord>(Shelf));
		}
		Status = FText::Format(LOCTEXT("Count", "{0} open shelves."), FText::AsNumber(Shelves.Num()));
	}
	else
	{
		const FSourceControlResultInfo& Info = Operation->GetResultInfo();
		Status = Info.ErrorMessages.Num() > 0 ? Info.ErrorMessages[0] : LOCTEXT("ListFailed", "Could not list shelves.");
	}
	ListView->RequestListRefresh();
}

FReply SLodeShelves::OnUnshelve()
{
	if (!Selected.IsValid())
	{
		return FReply::Handled();
	}
	TSharedRef<FLodeUnshelveShelf, ESPMode::ThreadSafe> Operation = ISourceControlOperation::Create<FLodeUnshelveShelf>();
	Operation->ShelfId = Selected->Id;
	bBusy = true;
	const ECommandResult::Type Result = FLodeSourceControlModule::Get().GetProvider().Execute(Operation, FSourceControlChangelistPtr(), Selected->Filenames, EConcurrency::Synchronous);
	bBusy = false;
	const FSourceControlResultInfo& Info = Operation->GetResultInfo();
	if (Result == ECommandResult::Succeeded)
	{
		Status = Info.InfoMessages.Num() > 0 ? Info.InfoMessages[0] : LOCTEXT("Unshelved", "Unshelved.");
		if (Info.ErrorMessages.Num() > 0)
		{
			FMessageDialog::Open(EAppMsgType::Ok, Info.ErrorMessages[0]);
		}
	}
	else
	{
		Status = Info.ErrorMessages.Num() > 0 ? Info.ErrorMessages[0] : LOCTEXT("UnshelveFailed", "Could not unshelve.");
	}
	return FReply::Handled();
}

#undef LOCTEXT_NAMESPACE
