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
#include "Transfer.h"
#include "mainDlg.h"
#include "langpack.h"
#include "settings.h"

Transfer::Transfer(CWnd* pParent /*=NULL*/)
: CThemeDialog(Transfer::IDD, pParent)
{
	Create (IDD, pParent);
}

Transfer::~Transfer(void)
{
}

int Transfer::OnCreate(LPCREATESTRUCT lpCreateStruct)
{
	if (langPack.rtl) {
		ModifyStyleEx(0,WS_EX_LAYOUTRTL);
	}
	return 0;
}

BOOL Transfer::OnInitDialog()
{
    m_bAllowDark = true;
	CThemeDialog::OnInitDialog();
	TranslateDialog(this->m_hWnd);

	CFont* font = this->GetFont();
	LOGFONT lf;
	font->GetLogFont(&lf);
	lf.lfHeight = MulDiv(16, dpiY, 96);
	m_font.CreateFontIndirect(&lf);

	CComboBox *combobox = (CComboBox*)GetDlgItem(IDC_NUMBER);
	combobox->SetFont(&m_font);

    GetDlgItem(IDC_KEY_1)->SetFont(&m_font);
	GetDlgItem(IDC_KEY_2)->SetFont(&m_font);
	GetDlgItem(IDC_KEY_3)->SetFont(&m_font);
	GetDlgItem(IDC_KEY_4)->SetFont(&m_font);
	GetDlgItem(IDC_KEY_5)->SetFont(&m_font);
	GetDlgItem(IDC_KEY_6)->SetFont(&m_font);
	GetDlgItem(IDC_KEY_7)->SetFont(&m_font);
	GetDlgItem(IDC_KEY_8)->SetFont(&m_font);
	GetDlgItem(IDC_KEY_9)->SetFont(&m_font);
	GetDlgItem(IDC_KEY_0)->SetFont(&m_font);
	GetDlgItem(IDC_KEY_STAR)->SetFont(&m_font);
	GetDlgItem(IDC_KEY_GRATE)->SetFont(&m_font);

	return TRUE;
}

void Transfer::SetDropDownWidth(CComboBox& combo)
{
    CDC* pDC = combo.GetDC();
    CFont* pOldFont = pDC->SelectObject(combo.GetFont());

    int maxWidth = 0;

    int count = combo.GetCount();
    for (int i = 0; i < count; ++i)
    {
        CString text;
        combo.GetLBText(i, text);

        CSize size = pDC->GetTextExtent(text);
        maxWidth = max(maxWidth, size.cx);
    }

    pDC->SelectObject(pOldFont);
    combo.ReleaseDC(pDC);

    maxWidth += GetSystemMetrics(SM_CXVSCROLL) + 10;

    combo.SetDroppedWidth(maxWidth);
}

void Transfer::LoadFromContacts(Contact *selectedContact)
{
	ClearDropdown();
	CComboBox *combobox = (CComboBox*)GetDlgItem(IDC_NUMBER);
	int selectedIndex = -1;
	bool hasStarred = false;
	POSITION pos = mainDlg->pageContacts->contacts.GetHeadPosition();
	while (pos) {
		Contact* contact = mainDlg->pageContacts->contacts.GetNext(pos);
		if (contact->starred) {
			hasStarred = true;
			break;
		}
	};
	pos = mainDlg->pageContacts->contacts.GetHeadPosition();
	int i = 0;
	while (pos) {
		Contact* contact = mainDlg->pageContacts->contacts.GetNext(pos);
		if (!hasStarred || contact->starred) {
			if (selectedContact == contact) {
				selectedIndex = i;
			}
            std::vector<CString> itemsNumber;
            std::vector<CString> itemsType;
            itemsNumber.push_back(contact->number);
            itemsType.push_back(_T(""));
            if (!contact->phone.IsEmpty()) {
                itemsNumber.push_back(contact->phone);
                itemsType.push_back(Translate(_T("Phone:")));
            }
            if (!contact->mobile.IsEmpty()) {
                itemsNumber.push_back(contact->mobile);
                itemsType.push_back(Translate(_T("Mobile:")));
            }
            for (size_t j = 0; j < itemsNumber.size(); ++j)
            {
                CString& number = itemsNumber[j];
                CString& type = itemsType[j];
                CString str;
                if (type.IsEmpty()) {
                    str.Format(_T("%s – %s"), contact->name, number);
                }
                else {
                    str.Format(_T("%s – %s %s"), contact->name, type, number);
                }
                int n = combobox->AddString(str);
                CString* pNumber = new CString(number);
                combobox->SetItemData(n, (DWORD_PTR)pNumber);
            }
			i++;
		}
	}
	if (selectedIndex != -1) {
		combobox->SetCurSel(selectedIndex);
	}
    SetDropDownWidth(*combobox);
}

void Transfer::ClearDropdown()
{
	CComboBox *combobox = (CComboBox*)GetDlgItem(IDC_NUMBER);
	int n = combobox->GetCount();
	for (int i = 0; i<n; i++) {
		CString *number = (CString *)combobox->GetItemData(i);
		delete number;
	}
	combobox->ResetContent();
}

void Transfer::OnDestroy()
{
	mainDlg->transferDlg = nullptr;
	ClearDropdown();
	CThemeDialog::OnDestroy();
}

void Transfer::PostNcDestroy()
{
	CThemeDialog::PostNcDestroy();
	delete this;
}

BEGIN_MESSAGE_MAP(Transfer, CThemeDialog)
	ON_WM_CREATE()
	ON_WM_SYSCOMMAND()
	ON_WM_CLOSE()
	ON_WM_DESTROY()
	ON_BN_CLICKED(IDOK, &Transfer::OnBnClickedOk)
	ON_BN_CLICKED(IDCANCEL, &Transfer::OnBnClickedCancel)
	ON_BN_CLICKED(IDC_TRANSFER_ATTENDED, &Transfer::OnBnClickedAttnded)
	ON_BN_CLICKED(IDC_TRANSFER_BLIND, &Transfer::OnBnClickedBlind)
	ON_BN_CLICKED(IDC_KEY_1, &Transfer::OnBnClickedKey1)
	ON_BN_CLICKED(IDC_KEY_2, &Transfer::OnBnClickedKey2)
	ON_BN_CLICKED(IDC_KEY_3, &Transfer::OnBnClickedKey3)
	ON_BN_CLICKED(IDC_KEY_4, &Transfer::OnBnClickedKey4)
	ON_BN_CLICKED(IDC_KEY_5, &Transfer::OnBnClickedKey5)
	ON_BN_CLICKED(IDC_KEY_6, &Transfer::OnBnClickedKey6)
	ON_BN_CLICKED(IDC_KEY_7, &Transfer::OnBnClickedKey7)
	ON_BN_CLICKED(IDC_KEY_8, &Transfer::OnBnClickedKey8)
	ON_BN_CLICKED(IDC_KEY_9, &Transfer::OnBnClickedKey9)
	ON_BN_CLICKED(IDC_KEY_STAR, &Transfer::OnBnClickedKeyStar)
	ON_BN_CLICKED(IDC_KEY_0, &Transfer::OnBnClickedKey0)
	ON_BN_CLICKED(IDC_KEY_GRATE, &Transfer::OnBnClickedKeyGrate)

END_MESSAGE_MAP()


void Transfer::OnSysCommand(UINT nID, LPARAM lParam)
{
	__super::OnSysCommand(nID, lParam);
}

void Transfer::OnClose() 
{
	if (mainDlg->transferDlg) {
		DestroyWindow();
	}
}

void Transfer::OnBnClickedOk()
{
	if (!GetDlgItem(IDOK)->IsWindowVisible()) {
		return;
	}
	if (Action(action)) {
		OnClose();
	}
}

void Transfer::OnBnClickedCancel()
{
	OnClose();
}

void Transfer::OnBnClickedAttnded()
{
	if (Action(MSIP_ACTION_ATTENDED_TRANSFER)) {
		OnClose();
	}
}

void Transfer::OnBnClickedBlind()
{
	if (Action(MSIP_ACTION_TRANSFER)) {
		OnClose();
	}
}

void Transfer::SetAction(msip_action action, pjsua_call_id call_id)
{
	this->action = action;
	callId = call_id;
	bool buttons = false;
	if (action == MSIP_ACTION_TRANSFER || action == MSIP_ACTION_ATTENDED_TRANSFER || action == MSIP_ACTION_FORWARD) {
		if (action == MSIP_ACTION_TRANSFER || action == MSIP_ACTION_ATTENDED_TRANSFER) {
			if (accountSettings.enableFeatureCodeAT
				&& !accountSettings.featureCodeAT.IsEmpty()
				) {
				buttons = true;
			}
		}
		if (buttons || action == MSIP_ACTION_FORWARD) {
			SetWindowText(Translate(_T("Call Transfer")));
		}
		else {
			if (action == MSIP_ACTION_ATTENDED_TRANSFER) {
				SetWindowText(Translate(_T("Attended Transfer")));
			}
			else {
				SetWindowText(Translate(_T("Blind Transfer")));
			}
		}
	}
	if (action == MSIP_ACTION_INVITE) {
		SetWindowText(Translate(_T("Invite to Conference")));
	}
	GetDlgItem(IDOK)->ShowWindow(!buttons ? SW_SHOW : SW_HIDE);
	GetDlgItem(IDC_TRANSFER_ATTENDED)->ShowWindow(buttons ? SW_SHOW : SW_HIDE);
	GetDlgItem(IDC_TRANSFER_BLIND)->ShowWindow(buttons ? SW_SHOW : SW_HIDE);
}

bool Transfer::Action(msip_action action)
{
	CString number;
	CComboBox* combobox = (CComboBox*)GetDlgItem(IDC_NUMBER);
	int i = combobox->GetCurSel();
	if (i == -1) {
		combobox->GetWindowText(number);
		number.Trim();
	}
	else {
		number = *(CString*)combobox->GetItemData(i);
	}
	if (!number.IsEmpty()) {
		mainDlg->messagesDlg->CallAction(action, number, callId);
		return true;
	}
	return false;
}

void Transfer::Input(CString digits)
{
	CComboBox *combobox = (CComboBox*)GetDlgItem(IDC_NUMBER);
	CEdit* edit = (CEdit*)FindWindowEx(combobox->m_hWnd, NULL, _T("EDIT"), NULL);
	if (edit) {
		int nLength = edit->GetWindowTextLength();
		edit->SetSel(nLength, nLength);
		edit->ReplaceSel(digits);
	}
}

void Transfer::OnBnClickedKey1()
{
	Input(_T("1"));
}

void Transfer::OnBnClickedKey2()
{
	Input(_T("2"));
}

void Transfer::OnBnClickedKey3()
{
	Input(_T("3"));
}

void Transfer::OnBnClickedKey4()
{
	Input(_T("4"));
}

void Transfer::OnBnClickedKey5()
{
	Input(_T("5"));
}

void Transfer::OnBnClickedKey6()
{
	Input(_T("6"));
}

void Transfer::OnBnClickedKey7()
{
	Input(_T("7"));
}

void Transfer::OnBnClickedKey8()
{
	Input(_T("8"));
}

void Transfer::OnBnClickedKey9()
{
	Input(_T("9"));
}

void Transfer::OnBnClickedKeyStar()
{
	Input(_T("*"));
}

void Transfer::OnBnClickedKey0()
{
	Input(_T("0"));
}

void Transfer::OnBnClickedKeyGrate()
{
	Input(_T("#"));
}
