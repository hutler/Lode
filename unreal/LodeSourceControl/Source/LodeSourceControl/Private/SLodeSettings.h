// Copyright SimpleDevOps contributors. Licensed under Apache-2.0.

#pragma once

#include "CoreMinimal.h"
#include "ISourceControlProvider.h"
#include "Widgets/DeclarativeSyntaxSupport.h"
#include "Widgets/SCompoundWidget.h"

class SEditableTextBox;

/**
 * The Lode part of the editor's revision control settings dialog: where lode is, what the
 * workspace points at (read-only), the refresh interval, "keep locks", and sign-in.
 */
class SLodeSettings : public SCompoundWidget
{
public:
	SLATE_BEGIN_ARGS(SLodeSettings) {}
	SLATE_END_ARGS()

	void Construct(const FArguments& InArgs);
	virtual ~SLodeSettings() override;

private:
	FText GetLodePath() const;
	void OnLodePathCommitted(const FText& InText, ETextCommit::Type InCommitType);
	FText GetWorkspaceText() const;
	FText GetProblemText() const;
	EVisibility GetProblemVisibility() const;
	int32 GetRefreshSeconds() const;
	void OnRefreshSecondsCommitted(int32 InValue, ETextCommit::Type InCommitType);
	ECheckBoxState GetKeepLocks() const;
	void OnKeepLocksChanged(ECheckBoxState InState);

	FReply OnSignInWithToken();
	FReply OnSignInWithPassword();
	FReply OnSignInWithBrowser();
	void StartLogin(uint8 Method);
	void OnLoginDone(const FSourceControlOperationRef& Operation, ECommandResult::Type Result);
	FText GetSignInStatus() const;
	bool IsSignInEnabled() const { return !bSigningIn; }

	TSharedPtr<SEditableTextBox> TokenBox;
	TSharedPtr<SEditableTextBox> NameBox;
	TSharedPtr<SEditableTextBox> PasswordBox;
	FText SignInStatus;
	bool bSigningIn = false;
	/** Cleared when the widget goes away, so a late completion does not touch it. */
	TSharedPtr<bool, ESPMode::ThreadSafe> bAlive;
};
