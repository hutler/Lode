// Copyright SimpleDevOps contributors. Licensed under Apache-2.0.

#include "SLodeSettings.h"

#include "ISourceControlModule.h"
#include "SourceControlOperations.h"
#include "Styling/AppStyle.h"
#include "Styling/StyleColors.h"
#include "Widgets/Input/SButton.h"
#include "Widgets/Input/SCheckBox.h"
#include "Widgets/Input/SEditableTextBox.h"
#include "Widgets/Input/SSpinBox.h"
#include "Widgets/Layout/SBox.h"
#include "Widgets/Layout/SSeparator.h"
#include "Widgets/SBoxPanel.h"
#include "Widgets/Text/STextBlock.h"

#include "LodeOperations.h"
#include "LodeProvider.h"
#include "LodeSourceControlModule.h"

#define LOCTEXT_NAMESPACE "LodeSourceControl.Settings"

namespace
{
	FLodeProvider& Provider()
	{
		return FLodeSourceControlModule::Get().GetProvider();
	}

	TSharedRef<SWidget> Row(const FText& Label, const FText& Tooltip, const TSharedRef<SWidget>& Value)
	{
		return SNew(SHorizontalBox)
			+ SHorizontalBox::Slot()
			.FillWidth(1.0f)
			.VAlign(VAlign_Center)
			.Padding(2.0f)
			[
				SNew(STextBlock)
				.Text(Label)
				.ToolTipText(Tooltip)
			]
			+ SHorizontalBox::Slot()
			.FillWidth(2.0f)
			.VAlign(VAlign_Center)
			.Padding(2.0f)
			[
				Value
			];
	}
}

void SLodeSettings::Construct(const FArguments& InArgs)
{
	bAlive = MakeShared<bool, ESPMode::ThreadSafe>(true);

	ChildSlot
	[
		SNew(SVerticalBox)
		+ SVerticalBox::Slot()
		.AutoHeight()
		.Padding(2.0f)
		[
			SNew(STextBlock)
			.Text(this, &SLodeSettings::GetProblemText)
			.Visibility(this, &SLodeSettings::GetProblemVisibility)
			.AutoWrapText(true)
			.ColorAndOpacity(FSlateColor(EStyleColor::Warning))
		]
		+ SVerticalBox::Slot()
		.AutoHeight()
		[
			Row(LOCTEXT("LodePath", "Path to lode"),
				LOCTEXT("LodePathTip", "The lode program. Leave empty to use the one on PATH."),
				SNew(SEditableTextBox)
				.Text(this, &SLodeSettings::GetLodePath)
				.HintText(LOCTEXT("LodePathHint", "lode (found on PATH)"))
				.OnTextCommitted(this, &SLodeSettings::OnLodePathCommitted))
		]
		+ SVerticalBox::Slot()
		.AutoHeight()
		[
			Row(LOCTEXT("Workspace", "Workspace"),
				LOCTEXT("WorkspaceTip", "Read from the Lode workspace that holds this project. Change it with the lode command."),
				SNew(STextBlock)
				.Text(this, &SLodeSettings::GetWorkspaceText)
				.AutoWrapText(true))
		]
		+ SVerticalBox::Slot()
		.AutoHeight()
		[
			Row(LOCTEXT("Refresh", "Refresh locks every (seconds)"),
				LOCTEXT("RefreshTip", "How often the editor asks the server who holds locks and which assets have newer versions, for the assets you are looking at."),
				SNew(SSpinBox<int32>)
				.MinValue(10)
				.MaxValue(3600)
				.Value(this, &SLodeSettings::GetRefreshSeconds)
				.OnValueCommitted(this, &SLodeSettings::OnRefreshSecondsCommitted))
		]
		+ SVerticalBox::Slot()
		.AutoHeight()
		[
			Row(LOCTEXT("KeepLocks", "Keep locks after submit"),
				LOCTEXT("KeepLocksTip", "Keep your locks on submitted assets, as if \"Keep files checked out\" were always ticked."),
				SNew(SCheckBox)
				.IsChecked(this, &SLodeSettings::GetKeepLocks)
				.OnCheckStateChanged(this, &SLodeSettings::OnKeepLocksChanged))
		]
		+ SVerticalBox::Slot()
		.AutoHeight()
		.Padding(2.0f, 6.0f)
		[
			SNew(SSeparator)
		]
		+ SVerticalBox::Slot()
		.AutoHeight()
		.Padding(2.0f)
		[
			SNew(STextBlock)
			.Text(LOCTEXT("SignInHeader", "Sign in (only if you are not signed in yet; lode keeps the sign-in, the editor does not)"))
			.AutoWrapText(true)
		]
		+ SVerticalBox::Slot()
		.AutoHeight()
		[
			Row(LOCTEXT("Token", "Token"),
				LOCTEXT("TokenTip", "An access token made on the Lode server (web admin or `lode admin token create`)."),
				SNew(SHorizontalBox)
				+ SHorizontalBox::Slot().FillWidth(1.0f)
				[
					SAssignNew(TokenBox, SEditableTextBox).IsPassword(true)
				]
				+ SHorizontalBox::Slot().AutoWidth().Padding(4.0f, 0.0f)
				[
					SNew(SButton)
					.Text(LOCTEXT("SignInToken", "Sign in"))
					.IsEnabled(this, &SLodeSettings::IsSignInEnabled)
					.OnClicked(this, &SLodeSettings::OnSignInWithToken)
				])
		]
		+ SVerticalBox::Slot()
		.AutoHeight()
		[
			Row(LOCTEXT("NamePassword", "Name and password"),
				LOCTEXT("NamePasswordTip", "Your Lode user name and password."),
				SNew(SHorizontalBox)
				+ SHorizontalBox::Slot().FillWidth(1.0f)
				[
					SAssignNew(NameBox, SEditableTextBox).HintText(LOCTEXT("NameHint", "name"))
				]
				+ SHorizontalBox::Slot().FillWidth(1.0f).Padding(4.0f, 0.0f, 0.0f, 0.0f)
				[
					SAssignNew(PasswordBox, SEditableTextBox).IsPassword(true).HintText(LOCTEXT("PasswordHint", "password"))
				]
				+ SHorizontalBox::Slot().AutoWidth().Padding(4.0f, 0.0f)
				[
					SNew(SButton)
					.Text(LOCTEXT("SignInPassword", "Sign in"))
					.IsEnabled(this, &SLodeSettings::IsSignInEnabled)
					.OnClicked(this, &SLodeSettings::OnSignInWithPassword)
				])
		]
		+ SVerticalBox::Slot()
		.AutoHeight()
		[
			Row(LOCTEXT("Browser", "Company sign-in"),
				LOCTEXT("BrowserTip", "Single sign-on through your company's identity provider, in the browser (when the server offers it)."),
				SNew(SHorizontalBox)
				+ SHorizontalBox::Slot().AutoWidth()
				[
					SNew(SButton)
					.Text(LOCTEXT("SignInBrowser", "Sign in with the browser"))
					.IsEnabled(this, &SLodeSettings::IsSignInEnabled)
					.OnClicked(this, &SLodeSettings::OnSignInWithBrowser)
				])
		]
		+ SVerticalBox::Slot()
		.AutoHeight()
		.Padding(2.0f)
		[
			SNew(STextBlock)
			.Text(this, &SLodeSettings::GetSignInStatus)
			.AutoWrapText(true)
		]
	];
}

SLodeSettings::~SLodeSettings()
{
	if (bAlive.IsValid())
	{
		*bAlive = false;
	}
}

FText SLodeSettings::GetLodePath() const
{
	return FText::FromString(Provider().GetSettings().GetLodePath());
}

void SLodeSettings::OnLodePathCommitted(const FText& InText, ETextCommit::Type InCommitType)
{
	FLodeProvider& LodeProvider = Provider();
	LodeProvider.AccessSettings().SetLodePath(InText.ToString());
	LodeProvider.AccessSettings().Save();
	// Find lode and the workspace again; the dialog's Accept button then connects.
	LodeProvider.Init(false);
}

FText SLodeSettings::GetWorkspaceText() const
{
	const FLodeProvider& LodeProvider = Provider();
	const FLodeWorkspaceInfo& Workspace = LodeProvider.GetWorkspace();
	if (Workspace.Root.IsEmpty())
	{
		return LOCTEXT("NoWorkspace", "No Lode workspace holds this project.");
	}
	return FText::Format(LOCTEXT("WorkspaceFormat", "{0}\nServer: {1}\nDepot: {2}\nBranch: {3}\nUser: {4}"),
		FText::FromString(Workspace.Root),
		FText::FromString(Workspace.Server.IsEmpty() ? TEXT("(connect to see)") : Workspace.Server),
		FText::FromString(Workspace.Depot.IsEmpty() ? TEXT("(connect to see)") : Workspace.Depot),
		FText::FromString(Workspace.Branch.IsEmpty() ? TEXT("(connect to see)") : Workspace.Branch),
		FText::FromString(Workspace.UserName.IsEmpty() ? TEXT("not signed in") : Workspace.UserName));
}

FText SLodeSettings::GetProblemText() const
{
	return FText::FromString(Provider().GetProblem());
}

EVisibility SLodeSettings::GetProblemVisibility() const
{
	return Provider().GetProblem().IsEmpty() ? EVisibility::Collapsed : EVisibility::Visible;
}

int32 SLodeSettings::GetRefreshSeconds() const
{
	return Provider().GetSettings().GetServerRefreshSeconds();
}

void SLodeSettings::OnRefreshSecondsCommitted(int32 InValue, ETextCommit::Type InCommitType)
{
	Provider().AccessSettings().SetServerRefreshSeconds(InValue);
	Provider().AccessSettings().Save();
}

ECheckBoxState SLodeSettings::GetKeepLocks() const
{
	return Provider().GetSettings().GetKeepLocksDefault() ? ECheckBoxState::Checked : ECheckBoxState::Unchecked;
}

void SLodeSettings::OnKeepLocksChanged(ECheckBoxState InState)
{
	Provider().AccessSettings().SetKeepLocksDefault(InState == ECheckBoxState::Checked);
	Provider().AccessSettings().Save();
}

FReply SLodeSettings::OnSignInWithToken()
{
	StartLogin(static_cast<uint8>(FLodeLogin::EMethod::Token));
	return FReply::Handled();
}

FReply SLodeSettings::OnSignInWithPassword()
{
	StartLogin(static_cast<uint8>(FLodeLogin::EMethod::Password));
	return FReply::Handled();
}

FReply SLodeSettings::OnSignInWithBrowser()
{
	StartLogin(static_cast<uint8>(FLodeLogin::EMethod::SingleSignOn));
	return FReply::Handled();
}

void SLodeSettings::StartLogin(uint8 Method)
{
	FLodeProvider& LodeProvider = Provider();
	if (!LodeProvider.IsEnabled())
	{
		SignInStatus = FText::FromString(LodeProvider.GetProblem());
		return;
	}

	TSharedRef<FLodeLogin, ESPMode::ThreadSafe> Operation = ISourceControlOperation::Create<FLodeLogin>();
	Operation->Method = static_cast<FLodeLogin::EMethod>(Method);
	switch (Operation->Method)
	{
	case FLodeLogin::EMethod::Token:
		Operation->Secret = TokenBox->GetText().ToString().TrimStartAndEnd();
		TokenBox->SetText(FText::GetEmpty());
		if (Operation->Secret.IsEmpty())
		{
			SignInStatus = LOCTEXT("NeedToken", "Paste a token first.");
			return;
		}
		break;
	case FLodeLogin::EMethod::Password:
		Operation->UserName = NameBox->GetText().ToString().TrimStartAndEnd();
		Operation->Secret = PasswordBox->GetText().ToString();
		PasswordBox->SetText(FText::GetEmpty());
		if (Operation->UserName.IsEmpty() || Operation->Secret.IsEmpty())
		{
			SignInStatus = LOCTEXT("NeedPassword", "Type your name and password first.");
			return;
		}
		break;
	case FLodeLogin::EMethod::SingleSignOn:
		SignInStatus = LOCTEXT("BrowserOpened", "Finish signing in in the browser window that opened...");
		break;
	}

	bSigningIn = true;
	if (Operation->Method != FLodeLogin::EMethod::SingleSignOn)
	{
		SignInStatus = LOCTEXT("SigningIn", "Signing in...");
	}
	TWeakPtr<bool, ESPMode::ThreadSafe> WeakAlive = bAlive;
	SLodeSettings* Self = this;
	LodeProvider.Execute(Operation, FSourceControlChangelistPtr(), TArray<FString>(), EConcurrency::Asynchronous,
		FSourceControlOperationComplete::CreateLambda([WeakAlive, Self](const FSourceControlOperationRef& Done, ECommandResult::Type Result)
		{
			const TSharedPtr<bool, ESPMode::ThreadSafe> Alive = WeakAlive.Pin();
			if (Alive.IsValid() && *Alive)
			{
				Self->OnLoginDone(Done, Result);
			}
			else if (Result == ECommandResult::Succeeded)
			{
				Provider().Execute(ISourceControlOperation::Create<FConnect>(), FSourceControlChangelistPtr(), TArray<FString>(), EConcurrency::Asynchronous);
			}
		}));
}

void SLodeSettings::OnLoginDone(const FSourceControlOperationRef& Operation, ECommandResult::Type Result)
{
	bSigningIn = false;
	if (Result == ECommandResult::Succeeded)
	{
		const TSharedRef<FLodeLogin, ESPMode::ThreadSafe> Login = StaticCastSharedRef<FLodeLogin>(Operation);
		SignInStatus = FText::Format(LOCTEXT("SignedInAs", "Signed in as {0}. Press Accept Settings to connect."), FText::FromString(Login->SignedInAs));
		// Connect again so the panel shows the user.
		Provider().Execute(ISourceControlOperation::Create<FConnect>(), FSourceControlChangelistPtr(), TArray<FString>(), EConcurrency::Asynchronous);
	}
	else
	{
		const FText Error = Operation->GetResultInfo().ErrorMessages.Num() > 0
			? Operation->GetResultInfo().ErrorMessages[0]
			: LOCTEXT("SignInFailed", "Sign-in failed.");
		SignInStatus = FText::Format(LOCTEXT("SignInFailedFormat", "Sign-in failed: {0}"), Error);
	}
}

FText SLodeSettings::GetSignInStatus() const
{
	return SignInStatus;
}

#undef LOCTEXT_NAMESPACE
