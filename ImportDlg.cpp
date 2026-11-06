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
#include "ImportDlg.h"

#include "mainDlg.h"
#include "settings.h"
#include "global.h"
#include "langpack.h"

ImportDlg::ImportDlg(CWnd* pParent /*=NULL*/)
    : CThemeDialog(ImportDlg::IDD, pParent)
{
    if (!Create(IDD, pParent)) {
        AfxMessageBox(_T("Failed to create import window on your system"));
        exit(0);
    }
}

ImportDlg::~ImportDlg(void)
{
}

int ImportDlg::OnCreate(LPCREATESTRUCT lpCreateStruct)
{
    if (langPack.rtl) {
        ModifyStyleEx(0, WS_EX_LAYOUTRTL);
    }
    return 0;
}

BOOL ImportDlg::OnInitDialog()
{
    m_bAllowDark = true;
    CThemeDialog::OnInitDialog();

    TranslateDialog(this->m_hWnd);

    return TRUE;
}

void ImportDlg::OnDestroy()
{
    mainDlg->importDlg = NULL;
    CThemeDialog::OnDestroy();
}

void ImportDlg::PostNcDestroy()
{
    CThemeDialog::PostNcDestroy();
    delete this;
}

BEGIN_MESSAGE_MAP(ImportDlg, CThemeDialog)
    ON_WM_CREATE()
    ON_WM_CLOSE()
    ON_WM_DESTROY()
    ON_BN_CLICKED(IDCANCEL, &ImportDlg::OnBnClickedCancel)
    ON_BN_CLICKED(IDOK, &ImportDlg::OnBnClickedOk)
    ON_WM_VKEYTOITEM()
    ON_NOTIFY(UDN_DELTAPOS, IDC_IMPORT_SPIN_MODIFY, &ImportDlg::OnDeltaposSpinModify)
    ON_NOTIFY(UDN_DELTAPOS, IDC_IMPORT_SPIN_ORDER, &ImportDlg::OnDeltaposSpinOrder)
END_MESSAGE_MAP()

void ImportDlg::OnClose()
{
    DestroyWindow();
}

void ImportDlg::OnBnClickedCancel()
{
    OnClose();
}

void ImportDlg::Init(CString filename)
{
    inFilename = filename;

    CListBox* listbox2;
    listbox2 = (CListBox*)GetDlgItem(IDC_IMPORT_ACCOUNTS);

    Account acc;
    int i = 0;
    CString str;
    while (true) {
        if (!accountSettings.AccountLoad(i, &acc, inFilename)) {
            if (i > 0) {
                break;
            }
        }
        else {
            str.Format(_T("%d: %s"), i, GetAccountLabel(&acc, i));
            listbox2->AddString(str);
        }
        i++;
    }
    InitCheckbox(_T("Settings"), IDC_EXPORT_SETTINGS);
    InitCheckbox(_T("Shortcuts"), IDC_EXPORT_SHORTCUTS);
}

void ImportDlg::InitCheckbox(CString section, int nID)
{
    CButton* checkbox = (CButton*)GetDlgItem(nID);
    TCHAR buffer[3];
    if (GetPrivateProfileString(section, NULL, NULL, buffer, 3, inFilename)) {
        checkbox->EnableWindow(TRUE);
        checkbox->SetCheck(1);
    }
    else {
        checkbox->EnableWindow(FALSE);
        checkbox->SetCheck(0);
    }
}

void ImportDlg::OnBnClickedOk()
{
    bool restart = false;
    bool activate = false;
    CListBox* listbox2;
    listbox2 = (CListBox*)GetDlgItem(IDC_IMPORT_ACCOUNTS);
    bool hasAccounts = listbox2->GetCount();
    bool hasSettings = ((CButton*)GetDlgItem(IDC_IMPORT_SETTINGS))->GetCheck();
    bool hasShortcuts = ((CButton*)GetDlgItem(IDC_IMPORT_SHORTCUTS))->GetCheck();
    if (hasAccounts || hasSettings || hasShortcuts) {
        if (hasSettings) {
            accountSettings.SettingsLoad(inFilename);
            restart = true;
        }
        if (hasShortcuts) {
            mainDlg->ShortcutsRemoveAll();
            ShortcutsLoad(inFilename);
            ShortcutsSave();

            if (!hasSettings) {
                if (!accountSettings.enableShortcuts) {
                    accountSettings.enableShortcuts = true;
                    restart = true;
                }
                else {
                    mainDlg->pageDialer->RebuildShortcuts();
                }
            }
        }
        if (hasAccounts) {
            Account dummy;
            int i = 1;
            while (true) {
                if (!accountSettings.AccountLoad(i, &dummy)) {
                    break;
                }
                i++;
            }
            int idCurr = i;
            bool hadAccounts = false;
            if (idCurr > 1) {
                hadAccounts = true;
            }
            for (unsigned i = 0; i < listbox2->GetCount(); i++)
            {
                CString value;
                listbox2->GetText(i, value);
                int pos = value.Find(':');
                int idRead = _wtoi(value.Left(pos));
                int idWrite;
                if (idRead == 0) {
                    idWrite = idRead;
                }
                else {
                    idWrite = idCurr;
                    idCurr++;
                }
                accountSettings.AccountLoad(idRead, &dummy, inFilename);
                accountSettings.AccountSave(idWrite, &dummy);
                if (!hadAccounts) {
                    activate = true;
                }
            }
        }
        if (restart) {
            if (activate) {
                accountSettings.accountId = 1;
                accountSettings.SettingsSave();
            }
            mainDlg->PostMessage(UM_RESTART, 0, 0);
        }
        else if (activate) {
            mainDlg->OnMenuAccountChange(ID_ACCOUNT_CHANGE_RANGE);
        }
        OnClose();
    }
}

int ImportDlg::OnVKeyToItem(UINT nKey, CListBox* pListBox, UINT nIndex)
{
    CListBox* listbox = (CListBox*)GetDlgItem(IDC_IMPORT_ACCOUNTS_ALL);
    CListBox* listbox2 = (CListBox*)GetDlgItem(IDC_IMPORT_ACCOUNTS);
    if (pListBox == listbox && listbox->GetCurSel() != -1) {
        if (nKey == 32) {
            //add
            NMUPDOWN NMUpDown;
            NMUpDown.iDelta = -1;
            LRESULT lResult;
            OnDeltaposSpinModify((NMHDR*)&NMUpDown, &lResult);
            return -2;
        }
    }
    if (pListBox == listbox2 && listbox2->GetCurSel() != -1) {
        if (nKey == 46) {
            //remove
            NMUPDOWN NMUpDown;
            NMUpDown.iDelta = 1;
            LRESULT lResult;
            OnDeltaposSpinModify((NMHDR*)&NMUpDown, &lResult);
            return -2;
        }
    }
    return -1;
}

void ImportDlg::OnDeltaposSpinModify(NMHDR* pNMHDR, LRESULT* pResult)
{
    LPNMUPDOWN pNMUpDown = reinterpret_cast<LPNMUPDOWN>(pNMHDR);
    CListBox* listbox;
    listbox = (CListBox*)GetDlgItem(IDC_IMPORT_ACCOUNTS_ALL);
    CListBox* listbox2;
    listbox2 = (CListBox*)GetDlgItem(IDC_IMPORT_ACCOUNTS);
    if (pNMUpDown->iDelta == -1) {
        //add
        int selected = listbox->GetCurSel();
        if (selected != LB_ERR)
        {
            CString str;
            listbox->GetText(selected, str);
            listbox2->AddString(str);
            listbox->DeleteString(selected);
            listbox->SetCurSel(selected < listbox->GetCount() ? selected : selected - 1);
        }
    }
    else {
        //remove
        int selected = listbox2->GetCurSel();
        if (selected != LB_ERR)
        {
            CString str;
            listbox2->GetText(selected, str);
            listbox->AddString(str);
            listbox2->DeleteString(selected);
            listbox2->SetCurSel(selected < listbox2->GetCount() ? selected : selected - 1);
        }
    }
    *pResult = 0;
}
void ImportDlg::OnDeltaposSpinOrder(NMHDR* pNMHDR, LRESULT* pResult)
{
    LPNMUPDOWN pNMUpDown = reinterpret_cast<LPNMUPDOWN>(pNMHDR);
    CListBox* listbox2;
    listbox2 = (CListBox*)GetDlgItem(IDC_IMPORT_ACCOUNTS);
    int selected = listbox2->GetCurSel();
    if (selected != LB_ERR)
    {
        CString str;
        listbox2->GetText(selected, str);
        if (pNMUpDown->iDelta == -1) {
            //up
            if (selected > 0)
            {
                listbox2->DeleteString(selected);
                listbox2->InsertString(selected - 1, str);
                listbox2->SetCurSel(selected - 1);
            }
        }
        else {
            //down
            if (selected < listbox2->GetCount() - 1)
            {
                listbox2->DeleteString(selected);
                listbox2->InsertString(selected + 1, str);
                listbox2->SetCurSel(selected + 1);
            }
        }
    }
    *pResult = 0;
}
