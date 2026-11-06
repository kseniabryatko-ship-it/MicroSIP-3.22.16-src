/*
 * Copyright (C) 2011-2026 MicroSIP (http://www.microsip.org)
 *
 * This program is free software; you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation; either version 2 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program; if not, write to the Free Software
 * Foundation, Inc., 59 Temple Place, Suite 330, Boston, MA  02111-1307  USA
 */

#include "stdafx.h"
#include "ShortcutsDlg.h"
#include "mainDlg.h"
#include "settings.h"
#include "langpack.h"

static int numberWidth = -1;
static int numberWidth2 = -1;
static int numberHeight = -1;

static CString defaultActionItems[] = {
    MSIP_SHORTCUT_CALL,
#ifdef _GLOBAL_VIDEO
    MSIP_SHORTCUT_VIDEOCALL,
#endif
    MSIP_SHORTCUT_MESSAGE,
    MSIP_SHORTCUT_DTMF,
    MSIP_SHORTCUT_BT,
    MSIP_SHORTCUT_AT,
    MSIP_SHORTCUT_CONF,
    MSIP_SHORTCUT_CALL_BT,
    MSIP_SHORTCUT_CALL_AT,
    MSIP_SHORTCUT_CALL_CONF,
};
static CString defaultActionValues[] = {
    _T("Call"),
#ifdef _GLOBAL_VIDEO
    _T("Video Call"),
#endif
    _T("Message"),
    _T("DTMF"),
    _T("Blind Transfer"),
    _T("Attended Transfer"),
    _T("Conference"),
    _T("Call/Blind Transfer"),
    _T("Call/Attended Transfer"),
    _T("Call/Conference"),

};

ShortcutsDlg::ShortcutsDlg(CWnd* pParent /*=NULL*/)
    : CThemeDialog(ShortcutsDlg::IDD, pParent)
{
    if (!Create(IDD, pParent)) {
        AfxMessageBox(_T("Failed to create shortcuts window"));
        exit(0);
    }
}

ShortcutsDlg::~ShortcutsDlg(void)
{
}

int ShortcutsDlg::OnCreate(LPCREATESTRUCT lpCreateStruct)
{
    if (langPack.rtl) {
        ModifyStyleEx(0, WS_EX_LAYOUTRTL);
    }
    return 0;
}

BOOL ShortcutsDlg::OnInitDialog()
{
    m_bAllowDark = true;
    CThemeDialog::OnInitDialog();

    TranslateDialog(this->m_hWnd);

    CString str;
    GetDlgItem(IDC_SHORTCUTS_LABEL2)->GetWindowText(str);
    str = str + L" 2";
    GetDlgItem(IDC_SHORTCUTS_LABEL2)->SetWindowText(str);
    
    GetDlgItem(IDC_SHORTCUTS_NUMBER2)->GetWindowText(str);
    str = str + L" 2";
    GetDlgItem(IDC_SHORTCUTS_NUMBER2)->SetWindowText(str);

    ((CButton*)GetDlgItem(IDC_SHORTCUTS_ENABLE))->SetCheck(accountSettings.enableShortcuts);
    ((CButton*)GetDlgItem(IDC_SHORTCUTS_BOTTOM))->SetCheck(accountSettings.shortcutsBottom);

    matrix = this;

    CRect rect;
    matrix->GetDlgItem(IDC_SHORTCUTS_EDIT_SHORTCUT_NUMBER)->GetWindowRect(rect);
    numberWidth = rect.Width();
    numberHeight = rect.Height();
    matrix->GetDlgItem(IDC_SHORTCUTS_EDIT_SHORTCUT_NUMBER2)->GetWindowRect(rect);
    numberWidth2 = rect.Width();
   
    PostMessage(WM_USER + 1, 0, 0);

    GetDlgItem(IDOK)->EnableWindow(FALSE);

    return TRUE;
}

LRESULT ShortcutsDlg::OnInitialied(WPARAM wParam, LPARAM lParam)
{
    bool ok = true;

    CStringList listActions;
    int n = sizeof(defaultActionItems) / sizeof(defaultActionItems[0]);
    for (int j = 0; j < n; j++) {
        CString str = defaultActionValues[j];
        int pos = str.Find('/');
        if (pos != -1) {
            CString str1 = Translate(str.Left(pos));
            CString str2 = Translate(str.Mid(pos + 1));
            listActions.AddTail(str1 + _T(" / ") + str2);
        }
        else {
            listActions.AddTail(Translate(str));
        }
    }

    int c = shortcuts.GetCount();
    CComboBox* combobox;
    for (int i = 0; i < _GLOBAL_SHORTCUTS_QTY && GetSafeHwnd(); i++) {
        combobox = (CComboBox*)matrix->GetDlgItem(IDC_SHORTCUTS_COMBO_SHORTCUT_TYPE + i);
        POSITION pos = listActions.GetHeadPosition();
        while (pos) {
            POSITION posKey = pos;
            CString str = listActions.GetNext(pos);
            combobox->AddString(str);
        };
        if (i < c) {
            Shortcut shortcut;
            shortcut = shortcuts.GetAt(i);
            matrix->GetDlgItem(IDC_SHORTCUTS_EDIT_SHORTCUT_LABEL + i)->SetWindowText(shortcut.label);
            matrix->GetDlgItem(IDC_SHORTCUTS_EDIT_SHORTCUT_NUMBER + i)->SetWindowText(shortcut.number);
            matrix->GetDlgItem(IDC_SHORTCUTS_EDIT_SHORTCUT_LABEL2 + i)->SetWindowText(shortcut.label2);
            matrix->GetDlgItem(IDC_SHORTCUTS_EDIT_SHORTCUT_NUMBER2 + i)->SetWindowText(shortcut.number2);
            int m = 0;
            for (int j = 0; j < n; j++) {
                if (shortcut.type == defaultActionItems[j]) {
                    m = j;
                }
            }
            combobox->SetCurSel(m);
            if (!shortcut.number2.IsEmpty()) {
                CButton* checkbox = (CButton*)matrix->GetDlgItem(IDC_SHORTCUTS_TOGGLE + i);
                checkbox->SetCheck(1);
                UpdateToggle(true, i);
            }
            CButton* checkbox = (CButton*)matrix->GetDlgItem(IDC_SHORTCUTS_PRESENCE + i);
            checkbox->SetCheck(shortcut.presence);
        }
        else {
            combobox->SetCurSel(0);
        }
        if (i == c || i == _GLOBAL_SHORTCUTS_QTY - 1) {
            if (ok && GetSafeHwnd()) {
                GetDlgItem(IDOK)->EnableWindow(TRUE);
            }
        }
        MSG msg;
        while (PeekMessage(&msg, NULL, 0, 0, PM_REMOVE))
        {
            TranslateMessage(&msg);
            DispatchMessage(&msg);
        }
    }
    return FALSE;
}

void ShortcutsDlg::OnDestroy()
{
    mainDlg->shortcutsDlg = nullptr;
    CThemeDialog::OnDestroy();
}

void ShortcutsDlg::PostNcDestroy()
{
    CThemeDialog::PostNcDestroy();
    delete this;
}

BEGIN_MESSAGE_MAP(ShortcutsDlg, CThemeDialog)
    ON_WM_CREATE()
    ON_WM_CLOSE()
    ON_WM_DESTROY()
    ON_MESSAGE(WM_USER + 1, OnInitialied)
    ON_COMMAND_RANGE(IDC_SHORTCUTS_TOGGLE, IDC_SHORTCUTS_TOGGLE + 99, OnBnClickedToggle)
    ON_NOTIFY(NM_CLICK, IDC_SHORTCUTS_SYSLINK_TYPE, &ShortcutsDlg::OnNMClickSyslinkType)
    ON_NOTIFY(NM_CLICK, IDC_SHORTCUTS_SYSLINK_TOGGLE, &ShortcutsDlg::OnNMClickSyslinkToggle)
    ON_NOTIFY(NM_CLICK, IDC_SHORTCUTS_SYSLINK_BLF, &ShortcutsDlg::OnNMClickSyslinkBLF)
    ON_BN_CLICKED(IDCANCEL, &ShortcutsDlg::OnBnClickedCancel)
    ON_BN_CLICKED(IDOK, &ShortcutsDlg::OnBnClickedOk)
END_MESSAGE_MAP()


void ShortcutsDlg::OnClose()
{
    DestroyWindow();
}

void ShortcutsDlg::OnBnClickedCancel()
{
    OnClose();
}

void ShortcutsDlg::OnBnClickedOk()
{
    this->ShowWindow(SW_HIDE);
    mainDlg->ShortcutsRemoveAll();
    for (int i = 0; i < _GLOBAL_SHORTCUTS_QTY; i++) {
        Shortcut shortcut;
        matrix->GetDlgItem(IDC_SHORTCUTS_EDIT_SHORTCUT_LABEL + i)->GetWindowText(shortcut.label);
        matrix->GetDlgItem(IDC_SHORTCUTS_EDIT_SHORTCUT_NUMBER + i)->GetWindowText(shortcut.number);
        matrix->GetDlgItem(IDC_SHORTCUTS_EDIT_SHORTCUT_LABEL2 + i)->GetWindowText(shortcut.label2);
        matrix->GetDlgItem(IDC_SHORTCUTS_EDIT_SHORTCUT_NUMBER2 + i)->GetWindowText(shortcut.number2);
        CButton* checkbox = (CButton*)matrix->GetDlgItem(IDC_SHORTCUTS_TOGGLE + i);
        if (!checkbox->GetCheck()) {
            shortcut.label2.Empty();
            shortcut.number2.Empty();
        }
        shortcut.presence = ((CButton*)matrix->GetDlgItem(IDC_SHORTCUTS_PRESENCE + i))->GetState();
        int n = ((CComboBox*)matrix->GetDlgItem(IDC_SHORTCUTS_COMBO_SHORTCUT_TYPE + i))->GetCurSel();
        if (n >= 0 && !shortcut.label.IsEmpty() &&
            (!shortcut.number.IsEmpty() ||
                defaultActionItems[n] == MSIP_SHORTCUT_BT ||
                defaultActionItems[n] == MSIP_SHORTCUT_AT)
            ) {
            if (shortcut.label2.IsEmpty()) {
                shortcut.label2 = shortcut.label;
            }
            shortcut.type = defaultActionItems[n];
            shortcuts.Add(shortcut);
        }
    }
    ShortcutsSave();

    accountSettings.enableShortcuts = ((CButton*)GetDlgItem(IDC_SHORTCUTS_ENABLE))->GetCheck();
    accountSettings.shortcutsBottom = ((CButton*)GetDlgItem(IDC_SHORTCUTS_BOTTOM))->GetCheck();

    mainDlg->pageDialer->RebuildShortcutsRestart();

    OnClose();
}

void ShortcutsDlg::OnBnClickedToggle(UINT nID)
{
    CButton* checkbox = (CButton*)matrix->GetDlgItem(nID);
    bool checked = checkbox->GetCheck();
    UpdateToggle(checked, nID - IDC_SHORTCUTS_TOGGLE);
    if (!checked) {
        bool hasChecked = false;
        for (int i = 0; i < _GLOBAL_SHORTCUTS_QTY; i++) {
            CButton* checkbox = (CButton*)matrix->GetDlgItem(IDC_SHORTCUTS_TOGGLE + i);
            if (checkbox->GetCheck()) {
                hasChecked = true;
                break;
            }
        }
        if (!hasChecked) {
            GetDlgItem(IDC_SHORTCUTS_LABEL2)->ShowWindow(SW_HIDE);
            GetDlgItem(IDC_SHORTCUTS_NUMBER2)->ShowWindow(SW_HIDE);
        }
    }
}

void ShortcutsDlg::OnNMClickSyslinkType(NMHDR* pNMHDR, LRESULT* pResult)
{
    OpenHelp(_T("shortcutType"));
    *pResult = 0;
}

void ShortcutsDlg::OnNMClickSyslinkToggle(NMHDR* pNMHDR, LRESULT* pResult)
{
    OpenHelp(_T("toggle"));
    *pResult = 0;
}

void ShortcutsDlg::OnNMClickSyslinkBLF(NMHDR* pNMHDR, LRESULT* pResult)
{
    OpenHelp(_T("BLF"));
    *pResult = 0;
}

void ShortcutsDlg::UpdateToggle(bool check, int i)
{
    if (check) {
        matrix->GetDlgItem(IDC_SHORTCUTS_EDIT_SHORTCUT_NUMBER + i)->SetWindowPos(NULL, 0, 0, numberWidth2, numberHeight, SWP_NOMOVE | SWP_NOZORDER);
        matrix->GetDlgItem(IDC_SHORTCUTS_EDIT_SHORTCUT_LABEL2 + i)->ShowWindow(SW_NORMAL);
        matrix->GetDlgItem(IDC_SHORTCUTS_EDIT_SHORTCUT_NUMBER2 + i)->ShowWindow(SW_NORMAL);
        GetDlgItem(IDC_SHORTCUTS_LABEL2)->ShowWindow(SW_SHOW);
        GetDlgItem(IDC_SHORTCUTS_NUMBER2)->ShowWindow(SW_SHOW);
    }
    else {
        matrix->GetDlgItem(IDC_SHORTCUTS_EDIT_SHORTCUT_NUMBER + i)->SetWindowPos(NULL, 0, 0, numberWidth, numberHeight, SWP_NOMOVE | SWP_NOZORDER);
        matrix->GetDlgItem(IDC_SHORTCUTS_EDIT_SHORTCUT_LABEL2 + i)->ShowWindow(SW_HIDE);
        matrix->GetDlgItem(IDC_SHORTCUTS_EDIT_SHORTCUT_NUMBER2 + i)->ShowWindow(SW_HIDE);
    }
}
