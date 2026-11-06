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
#include "ExportDlg.h"

#include "mainDlg.h"
#include "settings.h"
#include "global.h"
#include "langpack.h"

ExportDlg::ExportDlg(CWnd* pParent /*=NULL*/)
    : CThemeDialog(ExportDlg::IDD, pParent)
{
    if (!Create(IDD, pParent)) {
        AfxMessageBox(_T("Failed to create export window on your system"));
        exit(0);
    }
}

ExportDlg::~ExportDlg(void)
{
}

int ExportDlg::OnCreate(LPCREATESTRUCT lpCreateStruct)
{
    if (langPack.rtl) {
        ModifyStyleEx(0, WS_EX_LAYOUTRTL);
    }
    return 0;
}

BOOL ExportDlg::OnInitDialog()
{
    m_bAllowDark = true;
    CThemeDialog::OnInitDialog();

    TranslateDialog(this->m_hWnd);

    CListBox* listbox2;
    listbox2 = (CListBox*)GetDlgItem(IDC_IMPORT_ACCOUNTS);
    Account acc;
    int i = 0;
    if (!accountSettings.enableLocalAccount) {
        i = 1;
    }
    CString str;
    while (true) {
        if (!accountSettings.AccountLoad(i, &acc)) {
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

    return TRUE;
}

void ExportDlg::InitCheckbox(CString section, int nID)
{
    CButton* checkbox = (CButton*)GetDlgItem(nID);
    TCHAR buffer[3];
    if (GetPrivateProfileString(section, NULL, NULL, buffer, 3, accountSettings.iniFile)) {
        checkbox->EnableWindow(TRUE);
        checkbox->SetCheck(1);
    }
    else {
        checkbox->EnableWindow(FALSE);
        checkbox->SetCheck(0);
    }
}

void ExportDlg::OnDestroy()
{
    mainDlg->exportDlg = NULL;
    CThemeDialog::OnDestroy();
}

void ExportDlg::PostNcDestroy()
{
    CThemeDialog::PostNcDestroy();
    delete this;
}

BEGIN_MESSAGE_MAP(ExportDlg, CThemeDialog)
    ON_WM_CREATE()
    ON_WM_CLOSE()
    ON_WM_DESTROY()
    ON_BN_CLICKED(IDCANCEL, &ExportDlg::OnBnClickedCancel)
    ON_BN_CLICKED(IDOK, &ExportDlg::OnBnClickedOk)
    ON_WM_VKEYTOITEM()
    ON_NOTIFY(UDN_DELTAPOS, IDC_EXPORT_SPIN_MODIFY, &ExportDlg::OnDeltaposSpinModify)
    ON_NOTIFY(UDN_DELTAPOS, IDC_EXPORT_SPIN_ORDER, &ExportDlg::OnDeltaposSpinOrder)
END_MESSAGE_MAP()

void ExportDlg::OnClose()
{
    DestroyWindow();
}

void ExportDlg::OnBnClickedCancel()
{
    OnClose();
}

void ExportDlg::OnBnClickedOk()
{
    CListBox* listbox2;
    listbox2 = (CListBox*)GetDlgItem(IDC_EXPORT_ACCOUNTS);
    bool hasAccounts = listbox2->GetCount();
    bool hasSettings = ((CButton*)GetDlgItem(IDC_EXPORT_SETTINGS))->GetCheck();
    bool hasShortcuts = ((CButton*)GetDlgItem(IDC_EXPORT_SHORTCUTS))->GetCheck();
    if (hasAccounts || hasSettings || hasShortcuts) {
        TCHAR szFilters[] = _T("Ini Files (*.ini)|*.ini||");
        CFileDialog dlgFile(FALSE, _T("ini"), PathFindFileName(accountSettings.iniFile), OFN_PATHMUSTEXIST | OFN_OVERWRITEPROMPT | OFN_HIDEREADONLY, szFilters, this);
        if (dlgFile.DoModal() == IDOK) {
            CString filename = dlgFile.GetPathName();
            if (dlgFile.GetFileExt().IsEmpty()) {
                filename.Append(_T(".ini"));
            }
            WORD wBOM = 0xFEFF;
            CString sectionGlobal = _T("Global");
            CString pszSectionB;
            pszSectionB.Format(_T("[%s]"), sectionGlobal);
            CFile file;
            CFileException fileException;
            if (file.Open(filename, CFile::modeCreate | CFile::modeReadWrite, &fileException)) {
                file.Write(&wBOM, sizeof(wBOM));
                file.Write(pszSectionB.GetString(), pszSectionB.GetLength() * sizeof(wchar_t));
                file.Close();
            }
            WritePrivateProfileString(sectionGlobal, _T("version"), _T(_GLOBAL_VERSION), filename);
            if (hasSettings) {
                accountSettings.SettingsSave(filename);
            }
            if (hasShortcuts) {
                ShortcutsSave(filename);
            }
            if (hasAccounts) {
                int idCurr = 1;
                Account dummy;
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
                    accountSettings.AccountLoad(idRead, &dummy);
                    accountSettings.AccountSave(idWrite, &dummy, filename);
                }
            }
            OnClose();
        }
    }
}

int ExportDlg::OnVKeyToItem(UINT nKey, CListBox* pListBox, UINT nIndex)
{
    CListBox* listbox = (CListBox*)GetDlgItem(IDC_EXPORT_ACCOUNTS_ALL);
    CListBox* listbox2 = (CListBox*)GetDlgItem(IDC_EXPORT_ACCOUNTS);
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

void ExportDlg::OnDeltaposSpinModify(NMHDR* pNMHDR, LRESULT* pResult)
{
    LPNMUPDOWN pNMUpDown = reinterpret_cast<LPNMUPDOWN>(pNMHDR);
    CListBox* listbox;
    listbox = (CListBox*)GetDlgItem(IDC_EXPORT_ACCOUNTS_ALL);
    CListBox* listbox2;
    listbox2 = (CListBox*)GetDlgItem(IDC_EXPORT_ACCOUNTS);
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
void ExportDlg::OnDeltaposSpinOrder(NMHDR* pNMHDR, LRESULT* pResult)
{
    LPNMUPDOWN pNMUpDown = reinterpret_cast<LPNMUPDOWN>(pNMHDR);
    CListBox* listbox2;
    listbox2 = (CListBox*)GetDlgItem(IDC_EXPORT_ACCOUNTS);
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


